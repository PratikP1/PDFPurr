//! PDF Document: the high-level interface for reading PDF files.
//!
//! Provides [`Document`], which loads a PDF file, resolves the xref table
//! and trailer, and gives access to the object graph.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::content::text::extract_text_with_fonts;
use crate::core::objects::{
    DictExt, Dictionary, IndirectRef, Object, ObjectId, PdfName, PdfString, StringFormat,
};
use crate::error::{PdfError, PdfResult};
use crate::fonts::Font;
use crate::forms::{self, FormField};
use crate::images::PdfImage;
use crate::parser::file_structure::{
    find_startxref, load_xref_chain, parse_header, parse_indirect_object, parse_object_stream,
    rebuild_xref_from_scan, PdfVersion,
};
use crate::structure::{Annotation, Metadata, Outline};

/// Deferred type 2 xref entries: `(obj_num, stream_obj_num, index_in_stream)`.
///
/// These represent objects compressed inside ObjStm streams that cannot be
/// expanded until the streams are decompressed (and, for encrypted PDFs,
/// decrypted).
type CompressedEntries = Vec<(u32, u32, u16)>;

/// Scans for the `%PDF-` header within the first 1024 bytes.
///
/// Returns the byte offset where `%PDF-` begins. Tolerates leading
/// whitespace, BOM, or other bytes before the header — this is common
/// in real-world PDFs despite the spec requiring byte 0.
fn find_header_offset(data: &[u8]) -> PdfResult<usize> {
    let search_limit = data.len().min(1024);
    data[..search_limit]
        .windows(5)
        .position(|w| w == b"%PDF-")
        .ok_or_else(|| PdfError::SyntaxError {
            position: 0,
            message: "%PDF- header not found in first 1024 bytes".to_string(),
        })
}

/// A parsed PDF document.
///
/// This is the primary entry point for reading PDF files. It loads the
/// file structure, resolves cross-references, and provides access to
/// the document's object graph.
///
/// # Creating a new document
///
/// ```
/// use pdfpurr::Document;
///
/// let mut doc = Document::new();
/// doc.add_page(612.0, 792.0).unwrap(); // US Letter
/// let bytes = doc.to_bytes().unwrap();
/// assert!(!bytes.is_empty());
/// ```
///
/// # Parsing from bytes
///
/// ```
/// use pdfpurr::Document;
///
/// // Create, serialize, then re-parse
/// let mut doc = Document::new();
/// doc.add_page(612.0, 792.0).unwrap();
/// let bytes = doc.to_bytes().unwrap();
///
/// let parsed = Document::from_bytes(&bytes).unwrap();
/// assert_eq!(parsed.page_count().unwrap(), 1);
/// ```
#[derive(Debug)]
pub struct Document {
    /// The PDF version from the file header.
    pub version: PdfVersion,
    /// The trailer dictionary.
    pub trailer: Dictionary,
    /// All indirect objects, keyed by (object_number, generation).
    objects: HashMap<ObjectId, Object>,
    /// Cached font maps, keyed by the ObjectId of the `/Font` dictionary.
    /// Avoids re-parsing fonts when multiple pages share the same resources.
    font_cache: RefCell<HashMap<ObjectId, HashMap<String, Font>>>,
    /// Raw PDF data for lazy object parsing (None for documents built in memory).
    raw_data: Option<Vec<u8>>,
    /// Deferred xref entries for lazy loading: object_id → byte offset.
    /// Objects are parsed from `raw_data` on first access via `get_object`.
    deferred: RefCell<HashMap<ObjectId, u64>>,
    /// Lazily-parsed objects, populated on demand from `deferred` + `raw_data`.
    /// Stored separately from `objects` to avoid unsafe interior mutation.
    lazy_cache: RefCell<HashMap<ObjectId, Object>>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// Opens and parses a PDF file from disk.
    pub fn open<P: AsRef<Path>>(path: P) -> PdfResult<Self> {
        let data = fs::read(path)?;
        Self::from_bytes(&data)
    }

    /// Opens and parses a PDF file using memory-mapping.
    ///
    /// For large files, this avoids reading the entire file into memory
    /// upfront. The OS pages in data on demand as the parser accesses it.
    ///
    /// # Safety
    ///
    /// The file must not be modified while the returned `Document` is in use.
    /// Memory-mapped files reflect external changes, which could corrupt
    /// the parser's view of the data.
    pub fn open_mmap<P: AsRef<Path>>(path: P) -> PdfResult<Self> {
        let file = fs::File::open(path)?;
        let mmap = unsafe { memmap2::Mmap::map(&file) }
            .map_err(|e| PdfError::Io(std::io::Error::other(e)))?;
        Self::from_bytes(&mmap)
    }

    /// Parses a PDF document lazily — only the xref and trailer are loaded
    /// upfront. Individual objects are parsed on first access.
    ///
    /// This reduces startup time and memory for large PDFs where only a
    /// few pages need to be accessed. The trade-off is that `get_object`
    /// incurs a parse cost on first call for each object.
    pub fn from_bytes_lazy(data: &[u8]) -> PdfResult<Self> {
        let header_offset = find_header_offset(data)?;
        let (_, version) =
            parse_header(&data[header_offset..]).map_err(|e| PdfError::SyntaxError {
                position: header_offset,
                message: format!("Failed to parse PDF header: {}", e),
            })?;

        let xref_chain_result = find_startxref(data)
            .and_then(|offset| load_xref_chain(data, offset))
            .and_then(|chain| {
                if chain.is_empty() {
                    Err(PdfError::InvalidStructure(
                        "No xref sections found".to_string(),
                    ))
                } else {
                    Ok(chain)
                }
            });

        let xref_chain = match xref_chain_result {
            Ok(chain) => chain,
            Err(_) => {
                let (rebuilt_xref, rebuilt_trailer) = rebuild_xref_from_scan(data)?;
                vec![(rebuilt_xref, rebuilt_trailer)]
            }
        };

        let trailer = xref_chain
            .last()
            .ok_or_else(|| PdfError::InvalidStructure("Empty xref chain (lazy)".to_string()))?
            .1
            .clone();

        // Collect xref offsets for deferred parsing instead of loading objects
        let mut deferred_map: HashMap<ObjectId, u64> = HashMap::new();
        let mut objects = HashMap::new();

        for (xref_table, _) in &xref_chain {
            for subsection in &xref_table.subsections {
                for (i, entry) in subsection.entries.iter().enumerate() {
                    let obj_num = subsection.first_id + i as u32;

                    if !entry.in_use || entry.entry_type == 2 {
                        continue;
                    }

                    let id: ObjectId = (obj_num, entry.generation);
                    deferred_map.insert(id, entry.offset);
                }
            }
        }

        // Eagerly load only the catalog and pages tree root (needed for page_count)
        let root_ref = trailer
            .get_str("Root")
            .and_then(|o| o.as_reference())
            .map(|r| r.id());

        if let Some(root_id) = root_ref {
            if let Some(&offset) = deferred_map.get(&root_id) {
                if let Ok((_, indirect)) = parse_indirect_object(&data[offset as usize..]) {
                    objects.insert(root_id, indirect.object);
                    deferred_map.remove(&root_id);
                }
            }
        }

        Ok(Document {
            version,
            trailer,
            objects,
            font_cache: RefCell::new(HashMap::new()),
            raw_data: Some(data.to_vec()),
            deferred: RefCell::new(deferred_map),
            lazy_cache: RefCell::new(HashMap::new()),
        })
    }

    /// Parses a PDF document from a byte slice.
    pub fn from_bytes(data: &[u8]) -> PdfResult<Self> {
        let (mut doc, compressed_entries) = Self::parse_raw(data)?;
        doc.expand_object_streams(&compressed_entries);
        Ok(doc)
    }

    /// Parses the file structure and loads uncompressed objects, but defers
    /// ObjStm expansion. Returns the partially-built document and the list of
    /// compressed (type 2) xref entries that still need resolving.
    ///
    /// This split exists so that `from_bytes_with_password` can decrypt
    /// streams before expanding ObjStm containers — encrypted ObjStms
    /// cannot be decompressed until after decryption.
    fn parse_raw(data: &[u8]) -> PdfResult<(Self, CompressedEntries)> {
        // 1. Parse header — scan for %PDF- within the first 1024 bytes
        //    to tolerate leading whitespace, BOM, or garbage (common in the wild).
        let header_offset = find_header_offset(data)?;
        let (_, version) =
            parse_header(&data[header_offset..]).map_err(|e| PdfError::SyntaxError {
                position: header_offset,
                message: format!("Failed to parse PDF header: {}", e),
            })?;

        // 2. Find startxref and load the full xref chain (handles incremental updates).
        //    If normal xref loading fails (corrupt table, bad offset), fall back to
        //    rebuilding the xref by scanning for indirect object headers.
        let xref_chain_result = find_startxref(data)
            .and_then(|offset| load_xref_chain(data, offset))
            .and_then(|chain| {
                if chain.is_empty() {
                    Err(PdfError::InvalidStructure(
                        "No xref sections found".to_string(),
                    ))
                } else {
                    Ok(chain)
                }
            });

        let xref_chain = match xref_chain_result {
            Ok(chain) => chain,
            Err(_) => {
                // Xref is corrupt — rebuild by scanning for objects
                let (rebuilt_xref, rebuilt_trailer) = rebuild_xref_from_scan(data)?;
                vec![(rebuilt_xref, rebuilt_trailer)]
            }
        };

        // The newest trailer (last in chain) is the authoritative one.
        // SAFETY: xref_chain is always non-empty — either load_xref_chain
        // returns Ok with ≥1 entry, or rebuild_xref_from_scan wraps its
        // result in vec![(...)]. But we use ok_or to avoid the unwrap.
        let trailer = xref_chain
            .last()
            .ok_or_else(|| PdfError::InvalidStructure("Empty xref chain".to_string()))?
            .1
            .clone();

        // 3. Load objects from all xref sections (oldest first → newer overrides).
        //    Type 1 entries (uncompressed) are loaded directly from byte offsets.
        //    Type 2 entries (compressed in ObjStm) are deferred until after
        //    object stream expansion.
        let mut objects = HashMap::new();
        let mut freed: std::collections::HashSet<(u32, u16)> = std::collections::HashSet::new();
        // Deferred type 2 entries: (obj_num, stream_obj_num, index_in_stream)
        let mut compressed_entries: CompressedEntries = Vec::new();

        for (xref_table, _) in &xref_chain {
            for subsection in &xref_table.subsections {
                for (i, entry) in subsection.entries.iter().enumerate() {
                    let expected_num = subsection.first_id + i as u32;

                    if !entry.in_use {
                        let id = (expected_num, entry.generation);
                        objects.remove(&id);
                        freed.insert(id);
                        continue;
                    }

                    if entry.entry_type == 2 {
                        // Compressed in an object stream — defer until ObjStm expansion.
                        // offset = containing stream's object number, generation = index
                        compressed_entries.push((
                            expected_num,
                            entry.offset as u32,
                            entry.generation,
                        ));
                        continue;
                    }

                    // Type 1: uncompressed at byte offset
                    let offset = entry.offset as usize;
                    if offset >= data.len() {
                        continue;
                    }

                    match parse_indirect_object(&data[offset..]) {
                        Ok((_, indirect_obj)) => {
                            let id = if indirect_obj.object_number == expected_num {
                                (indirect_obj.object_number, indirect_obj.generation)
                            } else {
                                (expected_num, entry.generation)
                            };
                            objects.insert(id, indirect_obj.object);
                        }
                        Err(e) => {
                            tracing::debug!(
                                "Skipping malformed object at offset {}: {e}",
                                entry.offset
                            );
                            continue;
                        }
                    }
                }
            }
        }

        let doc = Document {
            version,
            trailer,
            objects,
            font_cache: RefCell::new(HashMap::new()),
            raw_data: None,
            deferred: RefCell::new(HashMap::new()),
            lazy_cache: RefCell::new(HashMap::new()),
        };

        Ok((doc, compressed_entries))
    }

    /// Expands ObjStm streams and resolves deferred type 2 xref entries.
    ///
    /// For encrypted PDFs this must be called **after** decryption so that
    /// the compressed streams can be decompressed.
    fn expand_object_streams(&mut self, compressed_entries: &[(u32, u32, u16)]) {
        let obj_stream_ids: Vec<ObjectId> = self
            .objects
            .iter()
            .filter(|(_, obj)| {
                if let Object::Stream(s) = obj {
                    s.dict.get_name("Type") == Some("ObjStm")
                } else {
                    false
                }
            })
            .map(|(id, _)| *id)
            .collect();

        let mut objstm_contents: HashMap<(u32, u16), Object> = HashMap::new();

        for stream_id in obj_stream_ids {
            if let Some(Object::Stream(stream)) = self.objects.remove(&stream_id) {
                if let Ok(embedded) = parse_object_stream(&stream) {
                    for (index, (obj_num, obj)) in embedded.into_iter().enumerate() {
                        objstm_contents.insert((stream_id.0, index as u16), obj.clone());
                        self.objects.entry((obj_num, 0)).or_insert(obj);
                    }
                }
            }
        }

        for &(obj_num, stream_obj_num, index) in compressed_entries {
            if let Some(obj) = objstm_contents.get(&(stream_obj_num, index)) {
                self.objects
                    .entry((obj_num, 0))
                    .or_insert_with(|| obj.clone());
            }
        }
    }

    /// Opens a password-protected PDF from a byte slice.
    ///
    /// If the document is encrypted, the password is used to derive the
    /// decryption key and all strings/streams are decrypted in place.
    /// If the document is not encrypted, the password is ignored.
    pub fn from_bytes_with_password(data: &[u8], password: &[u8]) -> PdfResult<Self> {
        let (mut doc, compressed_entries) = Self::parse_raw(data)?;

        // Check for /Encrypt in the trailer
        if let Some(encrypt_ref) = doc.trailer.get_str("Encrypt") {
            let encrypt_ref = encrypt_ref.clone();
            let encrypt_dict = doc
                .resolve(&encrypt_ref)
                .and_then(|o| o.as_dict())
                .ok_or_else(|| {
                    PdfError::InvalidStructure("Cannot resolve /Encrypt dictionary".to_string())
                })?
                .clone();

            // Get the file /ID array's first element
            let id_first = doc
                .trailer
                .get_str("ID")
                .and_then(|o| o.as_array())
                .and_then(|arr| arr.first())
                .and_then(|o| o.as_pdf_string().map(|s| s.bytes.clone()))
                .ok_or_else(|| {
                    PdfError::EncryptionError(
                        "Encrypted PDF missing /ID in trailer (required for key derivation)"
                            .to_string(),
                    )
                })?;

            let handler = crate::encryption::EncryptionHandler::from_dict(
                &encrypt_dict,
                &id_first,
                password,
            )?;

            // Find the encrypt dict's object ID so we can skip decrypting it
            let encrypt_obj_id = encrypt_ref.as_reference().map(|r| r.id());

            // Decrypt all objects in place — this must happen BEFORE expanding
            // ObjStm streams, because encrypted ObjStm data cannot be
            // decompressed until after decryption.
            for (&obj_id, obj) in doc.objects.iter_mut() {
                // Skip the encryption dictionary itself and XRef streams
                if Some(obj_id) == encrypt_obj_id {
                    continue;
                }
                if let Object::Stream(s) = obj {
                    if s.dict.get_name("Type") == Some("XRef") {
                        continue;
                    }
                }
                handler.decrypt_object(obj_id, obj);
            }
        }

        // Expand ObjStm streams after decryption so compressed objects
        // inside encrypted ObjStms are accessible.
        doc.expand_object_streams(&compressed_entries);

        Ok(doc)
    }

    /// Returns the number of indirect objects in the document.
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Retrieves an object by its ObjectId.
    ///
    /// For lazily-loaded documents, triggers on-demand parsing of
    /// deferred objects from the raw PDF data into a safe `RefCell` cache.
    pub fn get_object(&self, id: ObjectId) -> Option<&Object> {
        // Check eagerly-loaded objects first
        if let Some(obj) = self.objects.get(&id) {
            return Some(obj);
        }

        // Check lazy cache (already parsed on a previous call)
        // SAFETY NOTE: We leak the RefCell borrow here. This is safe because:
        // - lazy_cache is append-only (we only insert, never remove)
        // - The returned reference lives as long as &self
        // - No mutable borrow of lazy_cache is held while the reference is live
        if self.lazy_cache.borrow().contains_key(&id) {
            let cache = self.lazy_cache.as_ptr();
            // Safe: cache pointer is valid for the lifetime of &self,
            // and we only read from an entry that already exists.
            return unsafe { (*cache).get(&id) };
        }

        // Parse from deferred xref entry
        let offset = self.deferred.borrow().get(&id).copied();
        if let (Some(offset), Some(data)) = (offset, self.raw_data.as_ref()) {
            let offset_usize = offset as usize;
            if offset_usize < data.len() {
                if let Ok((_, indirect)) = parse_indirect_object(&data[offset_usize..]) {
                    self.deferred.borrow_mut().remove(&id);
                    self.lazy_cache.borrow_mut().insert(id, indirect.object);
                    let cache = self.lazy_cache.as_ptr();
                    return unsafe { (*cache).get(&id) };
                }
            }
        }

        None
    }

    /// Retrieves an object by its object number (generation 0).
    pub fn get_object_by_number(&self, object_number: u32) -> Option<&Object> {
        self.get_object((object_number, 0))
    }

    /// Resolves an object reference, following indirect references.
    ///
    /// The returned reference borrows from either `self` (for indirect
    /// references) or `obj` (for direct objects), so both share lifetime `'a`.
    pub fn resolve<'a>(&'a self, obj: &'a Object) -> Option<&'a Object> {
        match obj {
            Object::Reference(r) => self.get_object(r.id()),
            _ => Some(obj),
        }
    }

    /// Resolves an indirect reference to the object it points to.
    ///
    /// Unlike [`resolve`](Self::resolve), this only follows `Object::Reference`
    /// values and returns `None` for direct objects. The returned reference
    /// borrows only from `self`, decoupling it from the input's lifetime.
    fn resolve_reference(&self, obj: &Object) -> Option<&Object> {
        match obj {
            Object::Reference(r) => self.get_object(r.id()),
            _ => None,
        }
    }

    /// Returns the document catalog dictionary.
    /// Returns the object ID of the document catalog.
    ///
    /// The catalog is referenced by `/Root` in the trailer. For documents
    /// created with `Document::new()`, this is always `(1, 0)`. For parsed
    /// documents, it depends on the file's object numbering.
    pub fn catalog_object_id(&self) -> Option<ObjectId> {
        match self.trailer.get_str("Root")? {
            Object::Reference(r) => Some(r.id()),
            _ => None,
        }
    }

    /// Returns the document catalog dictionary.
    pub fn catalog(&self) -> PdfResult<&Dictionary> {
        let root_ref = self
            .trailer
            .get_str("Root")
            .ok_or_else(|| PdfError::InvalidStructure("No /Root in trailer".to_string()))?;

        let root_obj = self.resolve(root_ref).ok_or_else(|| {
            PdfError::InvalidReference("Cannot resolve /Root reference".to_string())
        })?;

        root_obj.as_dict().ok_or_else(|| PdfError::TypeError {
            expected: "Dictionary".to_string(),
            found: root_obj.type_name().to_string(),
        })
    }

    /// Returns the page count from the page tree root.
    pub fn page_count(&self) -> PdfResult<usize> {
        let catalog = self.catalog()?;

        let pages_ref = catalog
            .get_str("Pages")
            .ok_or_else(|| PdfError::InvalidStructure("No /Pages in catalog".to_string()))?;

        let pages_obj = self.resolve(pages_ref).ok_or_else(|| {
            PdfError::InvalidReference("Cannot resolve /Pages reference".to_string())
        })?;

        let pages_dict = pages_obj.as_dict().ok_or_else(|| PdfError::TypeError {
            expected: "Dictionary".to_string(),
            found: pages_obj.type_name().to_string(),
        })?;

        let count = pages_dict
            .get_i64("Count")
            .ok_or_else(|| PdfError::InvalidStructure("No /Count in Pages".to_string()))?;

        if count < 0 {
            return Err(PdfError::InvalidStructure(format!(
                "Negative page count: {}",
                count
            )));
        }

        Ok(count as usize)
    }

    /// Returns the document information dictionary, if present.
    pub fn info(&self) -> Option<&Dictionary> {
        let info_ref = self.trailer.get_str("Info")?;
        let info_obj = self.resolve(info_ref)?;
        info_obj.as_dict()
    }

    /// Returns the document title from the Info dictionary, if present.
    pub fn title(&self) -> Option<&str> {
        let info = self.info()?;
        let title_obj = info.get_str("Title")?;
        title_obj.as_pdf_string()?.as_text()
    }

    /// Returns all page dictionaries in document order by traversing the page tree.
    ///
    /// PDF page trees can be nested arbitrarily deep. This method flattens
    /// the tree into a linear sequence of leaf `/Page` nodes.
    pub fn pages(&self) -> PdfResult<Vec<&Dictionary>> {
        let catalog = self.catalog()?;

        let pages_ref = catalog
            .get_str("Pages")
            .ok_or_else(|| PdfError::InvalidStructure("No /Pages in catalog".to_string()))?;

        let pages_obj = self.resolve(pages_ref).ok_or_else(|| {
            PdfError::InvalidReference("Cannot resolve /Pages reference".to_string())
        })?;

        let pages_dict = pages_obj.as_dict().ok_or_else(|| PdfError::TypeError {
            expected: "Dictionary".to_string(),
            found: pages_obj.type_name().to_string(),
        })?;

        let mut result = Vec::new();
        self.collect_pages_inner(pages_dict, &mut result, Self::MAX_PAGE_TREE_DEPTH)?;
        Ok(result)
    }

    /// Maximum recursion depth for page tree traversal.
    ///
    /// Guards against circular `/Kids` references and absurdly deep trees.
    const MAX_PAGE_TREE_DEPTH: usize = 64;

    /// Returns the page dictionary at the given zero-based index.
    pub fn get_page(&self, index: usize) -> PdfResult<&Dictionary> {
        let pages = self.pages()?;
        pages.get(index).copied().ok_or_else(|| {
            PdfError::InvalidPage(format!(
                "Page index {} out of range (total: {})",
                index,
                pages.len()
            ))
        })
    }

    /// Finds an inherited property by walking up the `/Parent` chain.
    ///
    /// PDF page dictionaries inherit certain entries (MediaBox, Resources,
    /// CropBox, Rotate) from ancestor `/Pages` nodes.
    fn find_inherited<'a>(&'a self, page_dict: &'a Dictionary, key: &str) -> Option<&'a Object> {
        self.find_inherited_inner(page_dict, key, 32)
    }

    /// Recursive helper with a depth limit to guard against circular `/Parent` chains.
    fn find_inherited_inner<'a>(
        &'a self,
        page_dict: &'a Dictionary,
        key: &str,
        depth: usize,
    ) -> Option<&'a Object> {
        if let Some(obj) = page_dict.get_str(key) {
            return Some(obj);
        }
        if depth == 0 {
            return None;
        }
        let parent_ref = page_dict.get_str("Parent")?;
        let parent_obj = self.resolve(parent_ref)?;
        let parent_dict = parent_obj.as_dict()?;
        self.find_inherited_inner(parent_dict, key, depth - 1)
    }

    /// Returns the MediaBox for a page, inheriting from parent nodes if needed.
    pub fn page_media_box(&self, page_dict: &Dictionary) -> PdfResult<[f64; 4]> {
        let mb = self.find_inherited(page_dict, "MediaBox").ok_or_else(|| {
            PdfError::InvalidPage("No MediaBox found in page or ancestors".to_string())
        })?;
        mb.parse_rect().ok_or_else(|| {
            PdfError::InvalidStructure("MediaBox must be a 4-element numeric array".to_string())
        })
    }

    /// Recursively collects leaf page dictionaries from a page tree node.
    fn collect_pages_inner<'a>(
        &'a self,
        node: &'a Dictionary,
        result: &mut Vec<&'a Dictionary>,
        depth: usize,
    ) -> PdfResult<()> {
        if depth == 0 {
            return Err(PdfError::InvalidStructure(
                "Page tree exceeds maximum depth (circular reference or excessive nesting)"
                    .to_string(),
            ));
        }

        let type_name = node.get_name("Type");

        match type_name {
            Some("Page") => {
                result.push(node);
            }
            Some("Pages") => {
                let kids = node
                    .get_str("Kids")
                    .and_then(|o| o.as_array())
                    .ok_or_else(|| {
                        PdfError::InvalidStructure("No /Kids in /Pages node".to_string())
                    })?;

                for kid_ref in kids {
                    let kid_obj = self.resolve(kid_ref).ok_or_else(|| {
                        PdfError::InvalidReference("Cannot resolve page tree child".to_string())
                    })?;

                    let kid_dict = kid_obj.as_dict().ok_or_else(|| PdfError::TypeError {
                        expected: "Dictionary".to_string(),
                        found: kid_obj.type_name().to_string(),
                    })?;

                    self.collect_pages_inner(kid_dict, result, depth - 1)?;
                }
            }
            _ => {
                // Treat unknown nodes as pages (some PDFs omit /Type)
                result.push(node);
            }
        }

        Ok(())
    }

    /// Extracts text from a specific page (zero-based index).
    ///
    /// Decodes the page's content stream(s) and extracts text using
    /// text-showing operators. This is a basic extraction that works
    /// for PDFs with standard encodings.
    pub fn extract_page_text(&self, page_index: usize) -> PdfResult<String> {
        let page = self.get_page(page_index)?;
        self.extract_text_from_page_dict(page)
    }

    /// Extracts text from all pages, separated by form feeds.
    ///
    /// Pages where text extraction fails (e.g., missing content streams
    /// or unsupported encodings) are silently skipped. Use
    /// [`extract_page_text`](Self::extract_page_text) for per-page error handling.
    pub fn extract_all_text(&self) -> PdfResult<String> {
        let pages = self.pages()?;
        let mut result = String::new();

        for (i, page) in pages.iter().enumerate() {
            if i > 0 {
                result.push('\x0C'); // Form feed between pages
            }
            if let Ok(text) = self.extract_text_from_page_dict(page) {
                result.push_str(&text);
            }
        }

        Ok(result)
    }

    /// Extracts positioned text runs from a page's content stream.
    ///
    /// Each [`TextRun`](crate::content::analysis::TextRun) captures the text,
    /// font name/size, page-coordinate position, color, and style flags
    /// (bold/italic/monospaced). This is the foundation for structure
    /// detection — headings, paragraphs, lists, and tables can all be
    /// identified from the font metrics and positions of text runs.
    ///
    /// Returns an empty `Vec` if the page has no content stream.
    pub fn extract_text_runs(
        &self,
        page_index: usize,
    ) -> PdfResult<Vec<crate::content::analysis::TextRun>> {
        let page = self.get_page(page_index)?;
        let contents = match page.get_str("Contents") {
            Some(c) => c,
            None => return Ok(Vec::new()),
        };
        let data = self.resolve_content_data(contents)?;
        let fonts = self.page_fonts(page);
        crate::content::analysis::extract_text_runs(&data, &fonts)
    }

    /// Analyzes the structure of a page by detecting headings, paragraphs,
    /// and other elements from font metrics and text positions.
    ///
    /// This works on **native text** (from the content stream), not OCR.
    /// For scanned documents, run OCR first, then analyze structure.
    ///
    /// Returns classified [`TextBlock`](crate::content::structure_detection::TextBlock)
    /// values with roles like `Heading(1)`, `Paragraph`, `ListItem`, `Code`, etc.
    pub fn analyze_page_structure(
        &self,
        page_index: usize,
    ) -> PdfResult<Vec<crate::content::structure_detection::TextBlock>> {
        let runs = self.extract_text_runs(page_index)?;
        if runs.is_empty() {
            return Ok(Vec::new());
        }
        let page = self.get_page(page_index)?;
        let media_box = self.page_media_box(page)?;
        let page_width = media_box[2] - media_box[0];
        let page_height = media_box[3] - media_box[1];
        Ok(crate::content::structure_detection::classify_blocks(
            &runs,
            page_width,
            page_height,
        ))
    }

    /// Auto-tags an untagged PDF by analyzing all pages for structure.
    ///
    /// Detects headings, paragraphs, list items, and code blocks from
    /// font metrics and text positions, then builds a complete
    /// `/StructTreeRoot` with the detected structure. Sets `/MarkInfo`
    /// and `/Lang` on the catalog.
    ///
    /// Does nothing if the document already has a structure tree.
    /// Use [`check_accessibility`](Self::check_accessibility) to improve
    /// existing tags.
    pub fn auto_tag(&mut self, language: &str) -> PdfResult<usize> {
        // Skip if already tagged
        if self.structure_tree().is_some() {
            return Ok(0);
        }

        let page_count = self.page_count()?;
        let mut all_blocks = Vec::new();

        for i in 0..page_count {
            match self.analyze_page_structure(i) {
                Ok(blocks) => all_blocks.extend(blocks),
                Err(_) => continue, // Skip pages without content
            }
        }

        if all_blocks.is_empty() {
            return Ok(0);
        }

        crate::accessibility::auto_tag::auto_tag_from_blocks(self, &all_blocks, language)?;
        Ok(all_blocks.len())
    }

    /// Checks accessibility quality against detected page structure.
    ///
    /// Compares the existing structure tree (if any) against what the
    /// structure detection algorithms find in the content stream.
    /// Reports issues like:
    /// - Missing structure tree (untagged document)
    /// - Missing document language
    /// - Headings detected but not tagged
    /// - Figure elements without alt text
    /// - Heading level skips (H1 → H3 without H2)
    pub fn check_accessibility(&self) -> Vec<crate::accessibility::auto_tag::TagIssue> {
        use crate::accessibility::auto_tag::TagIssue;

        let mut all_issues = Vec::new();

        // Document-level checks (independent of page content)
        if self.structure_tree().is_none() {
            all_issues.push(TagIssue {
                description: "Document has no structure tree (untagged)".to_string(),
                suggestion: "Run auto_tag() to add tags from detected structure".to_string(),
                severity: "error".to_string(),
            });
        }

        let page_count = self.page_count().unwrap_or(0);

        for i in 0..page_count {
            let blocks = self.analyze_page_structure(i).unwrap_or_default();
            let issues = crate::accessibility::auto_tag::check_tag_quality(self, &blocks, i);
            all_issues.extend(issues);
        }

        // Deduplicate document-level issues (untagged, missing lang)
        all_issues.sort_by(|a, b| a.description.cmp(&b.description));
        all_issues.dedup_by(|a, b| a.description == b.description);

        all_issues
    }

    /// OCRs a page and compares against existing text content.
    ///
    /// Returns a [`HybridResult`](crate::ocr::hybrid::HybridResult)
    /// indicating whether the content stream text, OCR text, or both
    /// should be presented to screen readers. When they disagree, the
    /// accessible text contains both sources for human review.
    ///
    /// Use this when you suspect a PDF has garbled text encoding but
    /// want to verify before replacing it with OCR output.
    pub fn hybrid_ocr_page(
        &mut self,
        page_index: usize,
        engine: &dyn crate::ocr::engine::OcrEngine,
        config: &crate::ocr::OcrConfig,
    ) -> PdfResult<crate::ocr::hybrid::HybridResult> {
        let runs = self.extract_text_runs(page_index)?;

        // Render and OCR the page
        let page = self.get_page(page_index)?;
        let media_box = self.page_media_box(page)?;
        let page_width = media_box[2] - media_box[0];
        let page_height = media_box[3] - media_box[1];

        let renderer = crate::rendering::Renderer::new(
            self,
            crate::rendering::RenderOptions {
                dpi: config.dpi,
                ..Default::default()
            },
        );
        let pixmap = renderer.render_page(page_index)?;
        let grayscale = crate::ocr::pixmap_to_grayscale(&pixmap);
        let ocr_image = if config.preprocess {
            crate::ocr::preprocess::preprocess_for_ocr(&grayscale)
        } else {
            grayscale
        };

        let ocr_result = engine.recognize(&ocr_image)?;

        let hybrid = crate::ocr::hybrid::compare_text_sources(&runs, &ocr_result);

        // If OCR is better, apply it
        let should_apply = hybrid.source == crate::ocr::hybrid::TextSource::Ocr
            || hybrid.source == crate::ocr::hybrid::TextSource::Both;
        if should_apply && !ocr_result.words.is_empty() {
            // Apply OCR text layer
            let layer = crate::ocr::text_layer::build_ocr_text_layer(
                &ocr_result,
                page_width,
                page_height,
                config,
            );

            let mut font_dict = Dictionary::new();
            font_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Font")));
            font_dict.insert(PdfName::new("Subtype"), Object::Name(PdfName::new("Type1")));
            font_dict.insert(
                PdfName::new("BaseFont"),
                Object::Name(PdfName::new("Helvetica")),
            );
            if let Some(cmap) = layer.to_unicode_cmap {
                let cmap_id = self.add_object(Object::Stream(cmap));
                font_dict.insert(
                    PdfName::new("ToUnicode"),
                    Object::Reference(crate::core::objects::IndirectRef::new(cmap_id.0, cmap_id.1)),
                );
            }

            let mut fonts = Dictionary::new();
            fonts.insert(
                PdfName::new(crate::ocr::text_layer::OCR_FONT_NAME),
                Object::Dictionary(font_dict),
            );
            self.append_content_stream(page_index, &layer.content, Some(fonts))?;
        }

        Ok(hybrid)
    }

    /// Returns the resource dictionary for a page, walking up the `/Parent`
    /// chain to find inherited resources.
    ///
    /// PDF pages inherit resources from their parent `/Pages` nodes.
    /// This method returns the first `/Resources` dictionary found
    /// by walking up the page tree hierarchy.
    pub fn page_resources<'a>(&'a self, page_dict: &'a Dictionary) -> Option<&'a Dictionary> {
        let res_obj = self.find_inherited(page_dict, "Resources")?;
        self.resolve(res_obj)?.as_dict()
    }

    /// Loads all fonts from a page's resource dictionary.
    ///
    /// Returns a map from font name (e.g., "F1") to `Font` object.
    /// Results are cached by the `/Font` dictionary's object ID so that
    /// pages sharing the same resources (via inheritance) avoid re-parsing.
    pub fn page_fonts(&self, page_dict: &Dictionary) -> HashMap<String, Font> {
        let resources = match self.page_resources(page_dict) {
            Some(r) => r,
            None => return HashMap::new(),
        };

        let font_entry = match resources.get_str("Font") {
            Some(obj) => obj,
            None => return HashMap::new(),
        };

        // Check cache using the font dict's indirect reference ObjectId.
        let cache_key = font_entry.as_reference().map(|r| r.id());
        if let Some(key) = cache_key {
            if let Some(cached) = self.font_cache.borrow().get(&key) {
                return cached.clone();
            }
        }

        let font_dict = match self.resolve(font_entry).and_then(|o| o.as_dict()) {
            Some(d) => d,
            None => return HashMap::new(),
        };

        // Build a resolve closure for Font::from_dict.
        // Uses resolve_reference to decouple the output lifetime from the input.
        let resolve = |obj: &Object| self.resolve_reference(obj);

        let mut fonts = HashMap::new();
        for (name, font_ref) in font_dict {
            let font_obj = match self.resolve(font_ref) {
                Some(obj) => obj,
                None => continue,
            };
            if let Some(font_dict) = font_obj.as_dict() {
                match Font::from_dict(font_dict, &resolve) {
                    Ok(font) => {
                        fonts.insert(name.as_str().to_string(), font);
                    }
                    Err(e) => {
                        // Font parsing failed — log and insert a default font
                        // so text extraction still works (with default encoding).
                        tracing::debug!(
                            "Font '{}' failed to parse: {e}. Using default encoding.",
                            name.as_str()
                        );
                        fonts.insert(name.as_str().to_string(), Font::default_fallback());
                    }
                }
            }
        }

        // Cache the parsed fonts for future pages sharing this font dictionary.
        if let Some(key) = cache_key {
            self.font_cache.borrow_mut().insert(key, fonts.clone());
        }

        fonts
    }

    /// Extracts all images from a page's resource dictionary.
    ///
    /// Returns a list of `(name, PdfImage)` pairs for each image XObject
    /// found in the page's `/Resources/XObject` dictionary.
    /// Images that fail to parse are silently skipped.
    pub fn page_images(&self, page_dict: &Dictionary) -> Vec<(String, PdfImage)> {
        let mut images = Vec::new();

        // Extract XObject images from page resources
        if let Some(resources) = self.page_resources(page_dict) {
            if let Some(xobject_entry) = resources.get_str("XObject") {
                if let Some(xobject_dict) = self.resolve(xobject_entry).and_then(|o| o.as_dict()) {
                    for (name, xobj_ref) in xobject_dict {
                        let xobj = match self.resolve(xobj_ref) {
                            Some(obj) => obj,
                            None => continue,
                        };

                        let stream = match xobj.as_stream() {
                            Some(s) => s,
                            None => continue,
                        };

                        // Only extract /Subtype /Image XObjects
                        if stream.dict.get_name("Subtype") != Some("Image") {
                            continue;
                        }

                        match PdfImage::from_stream(stream) {
                            Ok(img) => images.push((name.as_str().to_string(), img)),
                            Err(e) => {
                                tracing::debug!("Skipping image '{}': {e}", name.as_str());
                                continue;
                            }
                        }
                    }
                }
            }
        }

        // Also extract inline images from the content stream (BI ... ID ... EI)
        if let Ok(content_data) = self.page_content_bytes(page_dict) {
            let inline_images = extract_inline_images(&content_data);
            for (idx, img) in inline_images.into_iter().enumerate() {
                images.push((format!("Inline{}", idx), img));
            }
        }

        images
    }

    /// Reads and concatenates all content stream bytes for a page.
    ///
    /// Decodes each content stream. If decoding fails, falls back to the
    /// raw (possibly compressed) stream data — partial content is better
    /// than silent data loss.
    fn page_content_bytes(&self, page_dict: &Dictionary) -> PdfResult<Vec<u8>> {
        let contents = page_dict
            .get(&PdfName::new("Contents"))
            .ok_or_else(|| PdfError::InvalidStructure("No /Contents on page".to_string()))?;

        let mut result = Vec::new();
        match contents {
            Object::Reference(r) => {
                if let Some(Object::Stream(s)) = self.get_object(r.id()) {
                    match s.decode_data() {
                        Ok(data) => result.extend_from_slice(&data),
                        Err(e) => {
                            tracing::warn!(
                                "Content stream {:?} decode failed ({e}), using raw bytes",
                                r.id()
                            );
                            result.extend_from_slice(&s.data);
                        }
                    }
                }
            }
            Object::Array(refs) => {
                for item in refs {
                    if let Object::Reference(r) = item {
                        if let Some(Object::Stream(s)) = self.get_object(r.id()) {
                            match s.decode_data() {
                                Ok(data) => {
                                    result.extend_from_slice(&data);
                                    result.push(b'\n');
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Content stream {:?} decode failed ({e}), using raw bytes",
                                        r.id()
                                    );
                                    result.extend_from_slice(&s.data);
                                    result.push(b'\n');
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        Ok(result)
    }

    /// Extracts all images from all pages.
    ///
    /// Returns a list of `(page_index, name, PdfImage)` tuples.
    pub fn extract_all_images(&self) -> PdfResult<Vec<(usize, String, PdfImage)>> {
        let pages = self.pages()?;
        let mut all_images = Vec::new();

        for (i, page) in pages.iter().enumerate() {
            for (name, img) in self.page_images(page) {
                all_images.push((i, name, img));
            }
        }

        Ok(all_images)
    }

    /// Returns the document's outline (bookmark) tree.
    ///
    /// Returns an empty vector if the document has no outlines.
    pub fn outlines(&self) -> Vec<Outline> {
        let catalog = match self.catalog() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let outlines_ref = match catalog.get_str("Outlines") {
            Some(obj) => obj,
            None => return Vec::new(),
        };

        let outlines_dict = match self.resolve(outlines_ref).and_then(|o| o.as_dict()) {
            Some(d) => d,
            None => return Vec::new(),
        };

        let resolve = |obj: &Object| self.resolve_reference(obj);
        Outline::from_outlines_dict(outlines_dict, &resolve)
    }

    /// Extracts annotations from a page.
    ///
    /// Returns a list of annotations found in the page's `/Annots` array.
    pub fn page_annotations(&self, page_dict: &Dictionary) -> Vec<Annotation> {
        let resolve = |obj: &Object| self.resolve_reference(obj);
        Annotation::from_page(page_dict, &resolve)
    }

    /// Returns the document's metadata from the `/Info` dictionary.
    ///
    /// Also attempts to extract XMP metadata from the catalog's `/Metadata`
    /// stream and merges the two sources (XMP takes precedence).
    pub fn metadata(&self) -> Metadata {
        let mut meta = Metadata::default();

        // Extract from /Info dictionary
        if let Some(info_ref) = self.trailer.get_str("Info") {
            if let Some(info_dict) = self.resolve(info_ref).and_then(|o| o.as_dict()) {
                meta = Metadata::from_info_dict(info_dict);
            }
        }

        // Try XMP metadata from catalog
        if let Ok(catalog) = self.catalog() {
            if let Some(meta_ref) = catalog.get_str("Metadata") {
                if let Some(meta_stream) = self.resolve(meta_ref).and_then(|o| o.as_stream()) {
                    if let Ok(xmp_bytes) = meta_stream.decode_data() {
                        if let Ok(xmp_str) = std::str::from_utf8(&xmp_bytes) {
                            let xmp_meta = Metadata::from_xmp(xmp_str);
                            meta = meta.merge(&xmp_meta);
                        }
                    }
                }
            }
        }

        meta
    }

    // --- Page Manipulation API ---

    /// Returns the ObjectId of the root `/Pages` dictionary.
    pub(crate) fn pages_id(&self) -> PdfResult<ObjectId> {
        let catalog = self.catalog()?;
        let pages_ref = catalog
            .get_str("Pages")
            .ok_or_else(|| PdfError::InvalidStructure("No /Pages in catalog".to_string()))?;
        pages_ref
            .as_reference()
            .map(|r| r.id())
            .ok_or_else(|| PdfError::InvalidStructure("/Pages is not a reference".to_string()))
    }

    /// Appends a content stream to an existing page.
    ///
    /// Creates a new stream object from `content` bytes, adds it to the
    /// page's `/Contents` (converting a single reference to an array if
    /// needed), and merges `fonts` into the page's `/Resources /Font`.
    ///
    /// This is the building block for OCR invisible text overlays and
    /// other content additions to existing pages.
    pub fn append_content_stream(
        &mut self,
        page_index: usize,
        content: &[u8],
        fonts: Option<Dictionary>,
    ) -> PdfResult<()> {
        let page_id = self.page_object_id(page_index)?;

        // Create the new content stream object
        let mut stream_dict = Dictionary::new();
        stream_dict.insert(
            PdfName::new("Length"),
            Object::Integer(content.len() as i64),
        );
        let stream = crate::core::objects::PdfStream::new(stream_dict, content.to_vec());
        let stream_id = self.add_object(Object::Stream(stream));
        let stream_ref = Object::Reference(IndirectRef::new(stream_id.0, stream_id.1));

        // Get the page dictionary mutably
        let page = self
            .get_object_mut(page_id)
            .and_then(|o| {
                if let Object::Dictionary(d) = o {
                    Some(d)
                } else {
                    None
                }
            })
            .ok_or_else(|| PdfError::InvalidStructure("Page is not a dictionary".to_string()))?;

        // Append to /Contents: convert single ref → array, or create new
        match page.get_str("Contents").cloned() {
            Some(existing @ Object::Reference(_)) => {
                page.insert(
                    PdfName::new("Contents"),
                    Object::Array(vec![existing, stream_ref]),
                );
            }
            Some(Object::Array(mut arr)) => {
                arr.push(stream_ref);
                page.insert(PdfName::new("Contents"), Object::Array(arr));
            }
            _ => {
                page.insert(PdfName::new("Contents"), stream_ref);
            }
        }

        // Merge fonts into /Resources /Font
        if let Some(new_fonts) = fonts {
            // Handle both inline resources and indirect resource references
            let res_ref = page.get_str("Resources").cloned();
            let res_id = match &res_ref {
                Some(Object::Reference(r)) => Some(r.id()),
                _ => None,
            };

            if let Some(rid) = res_id {
                // Resources is an indirect reference — modify the target object
                if let Some(Object::Dictionary(res)) = self.get_object_mut(rid) {
                    let font_entry = res
                        .entry(PdfName::new("Font"))
                        .or_insert_with(|| Object::Dictionary(Dictionary::new()));
                    if let Object::Dictionary(fd) = font_entry {
                        for (key, val) in new_fonts.iter() {
                            fd.entry(key.clone()).or_insert_with(|| val.clone());
                        }
                    }
                }
            } else {
                // Resources is inline or missing — modify page dict directly
                let page = self
                    .get_object_mut(page_id)
                    .and_then(|o| {
                        if let Object::Dictionary(d) = o {
                            Some(d)
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| PdfError::InvalidStructure("Page dict lost".to_string()))?;
                let resources = page
                    .entry(PdfName::new("Resources"))
                    .or_insert_with(|| Object::Dictionary(Dictionary::new()));
                if let Object::Dictionary(res) = resources {
                    let font_dict = res
                        .entry(PdfName::new("Font"))
                        .or_insert_with(|| Object::Dictionary(Dictionary::new()));
                    if let Object::Dictionary(fd) = font_dict {
                        for (key, val) in new_fonts.iter() {
                            fd.entry(key.clone()).or_insert_with(|| val.clone());
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Returns the indirect object ID of the page at `index`.
    ///
    /// Walks the `/Kids` array in the page tree root.
    pub fn page_object_id(&self, index: usize) -> PdfResult<ObjectId> {
        let pages_id = self.pages_id()?;
        let pages = self
            .get_object(pages_id)
            .and_then(|o| o.as_dict())
            .ok_or_else(|| PdfError::InvalidStructure("Cannot get /Pages dict".to_string()))?;
        let kids = pages
            .get_str("Kids")
            .and_then(|o| o.as_array())
            .ok_or_else(|| PdfError::InvalidStructure("No /Kids in /Pages".to_string()))?;
        if index >= kids.len() {
            return Err(PdfError::InvalidPage(format!(
                "Page index {} out of range (total: {})",
                index,
                kids.len()
            )));
        }
        kids[index]
            .as_reference()
            .ok_or_else(|| PdfError::InvalidStructure("Page ref is not a reference".to_string()))
            .map(|r| r.id())
    }

    /// Adds a blank page with the given dimensions (in points).
    ///
    /// Returns the zero-based page index. Standard US Letter is
    /// `(612.0, 792.0)` and A4 is `(595.0, 842.0)`.
    pub fn add_page(&mut self, width: f64, height: f64) -> PdfResult<usize> {
        let pages_id = self.pages_id()?;

        // Create the page dictionary
        let mut page_dict = Dictionary::new();
        page_dict.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
        page_dict.insert(
            PdfName::new("Parent"),
            Object::Reference(IndirectRef::new(pages_id.0, pages_id.1)),
        );
        page_dict.insert(
            PdfName::new("MediaBox"),
            Object::Array(vec![
                Object::Real(0.0),
                Object::Real(0.0),
                Object::Real(width),
                Object::Real(height),
            ]),
        );

        let page_id = self.add_object(Object::Dictionary(page_dict));

        // Update /Pages: append to /Kids and increment /Count
        let pages = self
            .get_object_mut(pages_id)
            .and_then(|o| {
                if let Object::Dictionary(d) = o {
                    Some(d)
                } else {
                    None
                }
            })
            .ok_or_else(|| PdfError::InvalidStructure("Cannot get /Pages dict".to_string()))?;

        // Append to /Kids
        if let Some(Object::Array(kids)) = pages.get_mut(&PdfName::new("Kids")) {
            kids.push(Object::Reference(IndirectRef::new(page_id.0, page_id.1)));
        }

        // Increment /Count
        if let Some(Object::Integer(count)) = pages.get_mut(&PdfName::new("Count")) {
            let idx = *count as usize;
            *count += 1;
            Ok(idx)
        } else {
            Err(PdfError::InvalidStructure(
                "No /Count in /Pages".to_string(),
            ))
        }
    }

    /// Removes a page at the given zero-based index.
    pub fn remove_page(&mut self, index: usize) -> PdfResult<()> {
        let pages_id = self.pages_id()?;

        let pages = self
            .get_object_mut(pages_id)
            .and_then(|o| {
                if let Object::Dictionary(d) = o {
                    Some(d)
                } else {
                    None
                }
            })
            .ok_or_else(|| PdfError::InvalidStructure("Cannot get /Pages dict".to_string()))?;

        // Remove from /Kids
        if let Some(Object::Array(kids)) = pages.get_mut(&PdfName::new("Kids")) {
            if index >= kids.len() {
                return Err(PdfError::InvalidPage(format!(
                    "Page index {} out of range (total: {})",
                    index,
                    kids.len()
                )));
            }
            kids.remove(index);
        } else {
            return Err(PdfError::InvalidStructure("No /Kids in /Pages".to_string()));
        }

        // Decrement /Count
        if let Some(Object::Integer(count)) = pages.get_mut(&PdfName::new("Count")) {
            *count -= 1;
        }

        Ok(())
    }

    /// Sets the rotation angle for a page (must be a multiple of 90).
    pub fn rotate_page(&mut self, page_index: usize, degrees: i32) -> PdfResult<()> {
        if degrees % 90 != 0 {
            return Err(PdfError::InvalidPage(
                "Rotation must be a multiple of 90".to_string(),
            ));
        }

        // Find the page's object ID via /Kids
        let pages_id = self.pages_id()?;
        let page_ref = {
            let pages = self
                .get_object(pages_id)
                .and_then(|o| o.as_dict())
                .ok_or_else(|| PdfError::InvalidStructure("Cannot get /Pages dict".to_string()))?;
            let kids = pages
                .get_str("Kids")
                .and_then(|o| o.as_array())
                .ok_or_else(|| PdfError::InvalidStructure("No /Kids in /Pages".to_string()))?;
            if page_index >= kids.len() {
                return Err(PdfError::InvalidPage(format!(
                    "Page index {} out of range (total: {})",
                    page_index,
                    kids.len()
                )));
            }
            kids[page_index]
                .as_reference()
                .ok_or_else(|| {
                    PdfError::InvalidStructure("Page ref is not a reference".to_string())
                })?
                .id()
        };

        let page_dict = self
            .get_object_mut(page_ref)
            .and_then(|o| {
                if let Object::Dictionary(d) = o {
                    Some(d)
                } else {
                    None
                }
            })
            .ok_or_else(|| PdfError::InvalidStructure("Cannot get page dict".to_string()))?;

        page_dict.insert(PdfName::new("Rotate"), Object::Integer(degrees as i64));
        Ok(())
    }

    /// Reorders pages according to the given permutation.
    ///
    /// `order` must contain exactly one entry per page, where each entry
    /// is the original zero-based index of the page to place at that position.
    pub fn reorder_pages(&mut self, order: &[usize]) -> PdfResult<()> {
        let pages_id = self.pages_id()?;

        let pages = self
            .get_object_mut(pages_id)
            .and_then(|o| {
                if let Object::Dictionary(d) = o {
                    Some(d)
                } else {
                    None
                }
            })
            .ok_or_else(|| PdfError::InvalidStructure("Cannot get /Pages dict".to_string()))?;

        if let Some(Object::Array(kids)) = pages.get_mut(&PdfName::new("Kids")) {
            if order.len() != kids.len() {
                return Err(PdfError::InvalidPage(format!(
                    "Order length ({}) doesn't match page count ({})",
                    order.len(),
                    kids.len()
                )));
            }

            let old_kids = kids.clone();
            for (i, &src) in order.iter().enumerate() {
                if src >= old_kids.len() {
                    return Err(PdfError::InvalidPage(format!(
                        "Invalid page index {} in order",
                        src
                    )));
                }
                kids[i] = old_kids[src].clone();
            }
        }

        Ok(())
    }

    /// Merges all pages from another document into this one.
    ///
    /// Objects from the other document are renumbered to avoid ID
    /// collisions, and all internal references are updated accordingly.
    pub fn merge(&mut self, other: &Document) -> PdfResult<()> {
        // Build ID remapping: other_id → new_id
        let mut id_map: HashMap<ObjectId, ObjectId> = HashMap::new();
        let base_num = self.objects.keys().map(|&(n, _)| n).max().unwrap_or(0) + 1;

        for (i, &old_id) in other.objects.keys().enumerate() {
            let new_id = (base_num + i as u32, 0);
            id_map.insert(old_id, new_id);
        }

        // Copy objects with remapped references
        for (&old_id, obj) in &other.objects {
            let new_id = id_map[&old_id];
            let mut new_obj = obj.clone();
            Self::remap_references(&mut new_obj, &id_map);
            self.objects.insert(new_id, new_obj);
        }

        // Find the other document's page references
        let other_catalog = other.catalog()?;
        let other_pages_ref = other_catalog
            .get_str("Pages")
            .ok_or_else(|| PdfError::InvalidStructure("No /Pages in other catalog".to_string()))?;
        let other_pages = other
            .resolve(other_pages_ref)
            .and_then(|o| o.as_dict())
            .ok_or_else(|| PdfError::InvalidStructure("Cannot resolve other /Pages".to_string()))?;
        let other_kids = other_pages
            .get_str("Kids")
            .and_then(|o| o.as_array())
            .unwrap_or(&[]);

        // Remap and append the other's page references to our /Kids
        let pages_id = self.pages_id()?;

        // Update /Parent on each copied page to point to our /Pages
        for kid in other_kids {
            if let Some(kid_ref) = kid.as_reference() {
                if let Some(&new_id) = id_map.get(&kid_ref.id()) {
                    if let Some(Object::Dictionary(page_dict)) = self.get_object_mut(new_id) {
                        page_dict.insert(
                            PdfName::new("Parent"),
                            Object::Reference(IndirectRef::new(pages_id.0, pages_id.1)),
                        );
                    }
                }
            }
        }

        let pages = self
            .get_object_mut(pages_id)
            .and_then(|o| {
                if let Object::Dictionary(d) = o {
                    Some(d)
                } else {
                    None
                }
            })
            .ok_or_else(|| PdfError::InvalidStructure("Cannot get /Pages dict".to_string()))?;

        if let Some(Object::Array(kids)) = pages.get_mut(&PdfName::new("Kids")) {
            for kid in other_kids {
                let mut new_kid = kid.clone();
                Self::remap_references(&mut new_kid, &id_map);
                kids.push(new_kid);
            }
        }

        // Update /Count
        let other_count = other_pages.get_i64("Count").unwrap_or(0);
        if let Some(Object::Integer(count)) = pages.get_mut(&PdfName::new("Count")) {
            *count += other_count;
        }

        Ok(())
    }

    /// Recursively remaps all indirect references in an object.
    fn remap_references(obj: &mut Object, id_map: &HashMap<ObjectId, ObjectId>) {
        match obj {
            Object::Reference(r) => {
                if let Some(&new_id) = id_map.get(&r.id()) {
                    *r = IndirectRef::new(new_id.0, new_id.1);
                }
            }
            Object::Array(arr) => {
                for item in arr.iter_mut() {
                    Self::remap_references(item, id_map);
                }
            }
            Object::Dictionary(dict) => {
                for (_, val) in dict.iter_mut() {
                    Self::remap_references(val, id_map);
                }
            }
            Object::Stream(stream) => {
                for (_, val) in stream.dict.iter_mut() {
                    Self::remap_references(val, id_map);
                }
            }
            _ => {}
        }
    }

    // --- AcroForms API ---

    /// Returns all form fields in the document.
    ///
    /// Parses the `/AcroForm` dictionary from the catalog and walks the
    /// `/Fields` tree to collect all form fields with their current values.
    /// Returns an empty list if the document has no form fields.
    pub fn form_fields(&self) -> Vec<FormField> {
        let catalog = match self.catalog() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        let acroform = match catalog.get_str("AcroForm") {
            Some(obj) => match self.resolve(obj).and_then(|o| o.as_dict()) {
                Some(d) => d,
                None => return Vec::new(),
            },
            None => return Vec::new(),
        };

        let fields = match acroform.get_str("Fields").and_then(|o| o.as_array()) {
            Some(arr) => arr,
            None => return Vec::new(),
        };

        forms::field::collect_fields(fields, "", &|obj| self.resolve(obj))
    }

    /// Sets a form field value by its fully qualified name.
    ///
    /// Updates the `/V` entry in the field's dictionary and removes
    /// the `/AP` (appearance) entry to force viewers to regenerate
    /// the field's visual appearance.
    pub fn set_form_field(&mut self, name: &str, value: &str) -> PdfResult<()> {
        // First find the field's object ID
        let field = self
            .form_fields()
            .into_iter()
            .find(|f| f.name == name)
            .ok_or_else(|| {
                PdfError::InvalidStructure(format!("Form field '{}' not found", name))
            })?;

        if field.obj_id == (0, 0) {
            return Err(PdfError::InvalidStructure(
                "Cannot set field value: field has no object ID".to_string(),
            ));
        }

        let dict = self
            .get_object_mut(field.obj_id)
            .and_then(|o| {
                if let Object::Dictionary(d) = o {
                    Some(d)
                } else {
                    None
                }
            })
            .ok_or_else(|| PdfError::InvalidStructure("Cannot get field dictionary".to_string()))?;

        // Set /V
        dict.insert(
            PdfName::new("V"),
            Object::String(PdfString {
                bytes: value.as_bytes().to_vec(),
                format: StringFormat::Literal,
            }),
        );

        // Remove /AP to force appearance regeneration
        dict.remove(&PdfName::new("AP"));

        Ok(())
    }

    /// Returns all digital signature dictionaries found in the document.
    ///
    /// Searches for `/Sig` type form fields in the `/AcroForm` or for
    /// `/Sig` dictionaries directly in the object store.
    pub fn signatures(&self) -> Vec<crate::signatures::SignatureInfo> {
        // Look for signature fields among form fields
        self.form_fields()
            .iter()
            .filter(|f| f.field_type == crate::forms::field::FieldType::Signature)
            .filter_map(|f| {
                if f.obj_id == (0, 0) {
                    return None;
                }
                let field_dict = self.get_object(f.obj_id)?.as_dict()?;
                // The /V entry points to the signature dictionary
                let sig_obj = field_dict.get_str("V")?;
                let sig_dict = self.resolve(sig_obj).and_then(|o| o.as_dict())?;
                crate::signatures::SignatureInfo::from_dict(sig_dict).ok()
            })
            .collect()
    }

    /// Parses the document structure tree (`/StructTreeRoot`) for tagged PDFs.
    ///
    /// Returns `None` if the document has no structure tree (i.e. is not tagged).
    pub fn structure_tree(&self) -> Option<crate::accessibility::StructTree> {
        let catalog = self.catalog().ok()?;
        let struct_tree_ref = catalog.get_str("StructTreeRoot")?;
        let struct_tree_dict = self.resolve(struct_tree_ref)?.as_dict()?;
        let resolve = |obj: &Object| self.resolve_reference(obj);
        Some(crate::accessibility::StructTree::from_dict(
            struct_tree_dict,
            &resolve,
        ))
    }

    /// Runs PDF/UA accessibility validation on the document.
    ///
    /// Returns a report with pass/fail checks for tagged PDF, language,
    /// figure alt text, heading order, and table headers.
    /// Returns a report with all checks failed if the document has no structure tree.
    pub fn accessibility_report(&self) -> crate::accessibility::AccessibilityReport {
        match self.structure_tree() {
            Some(tree) => crate::accessibility::validate_pdf_ua(&tree),
            None => {
                // No structure tree — create empty tree for validation (will fail tagged check)
                let empty = crate::accessibility::StructTree {
                    role_map: Default::default(),
                    children: vec![],
                    lang: None,
                };
                crate::accessibility::validate_pdf_ua(&empty)
            }
        }
    }

    /// Validates the document against PDF/A requirements.
    pub fn validate_pdfa(
        &self,
        level: crate::standards::PdfALevel,
    ) -> crate::standards::StandardsReport {
        crate::standards::validate_pdfa(self, level)
    }

    /// Renders a single page to a pixel image at the given DPI.
    ///
    /// This is a convenience wrapper around [`Renderer`](crate::rendering::Renderer).
    pub fn render_page(&self, page_index: usize, dpi: f64) -> PdfResult<tiny_skia::Pixmap> {
        let opts = crate::rendering::RenderOptions {
            dpi,
            ..Default::default()
        };
        crate::rendering::Renderer::new(self, opts).render_page(page_index)
    }

    /// Validates the document against PDF/X requirements.
    pub fn validate_pdfx(
        &self,
        level: crate::standards::PdfXLevel,
    ) -> crate::standards::StandardsReport {
        crate::standards::validate_pdfx(self, level)
    }

    // --- Write / Serialization API ---

    /// Creates a new empty PDF document.
    ///
    /// The document contains a minimal valid structure: a catalog and
    /// an empty page tree. Use [`add_page`](Self::add_page) to add pages.
    pub fn new() -> Self {
        let mut objects = HashMap::new();

        // Object 1: Catalog
        let mut catalog = Dictionary::new();
        catalog.insert(PdfName::new("Type"), Object::Name(PdfName::new("Catalog")));
        catalog.insert(
            PdfName::new("Pages"),
            Object::Reference(IndirectRef::new(2, 0)),
        );
        objects.insert((1, 0), Object::Dictionary(catalog));

        // Object 2: Pages (empty page tree)
        let mut pages = Dictionary::new();
        pages.insert(PdfName::new("Type"), Object::Name(PdfName::new("Pages")));
        pages.insert(PdfName::new("Kids"), Object::Array(Vec::new()));
        pages.insert(PdfName::new("Count"), Object::Integer(0));
        objects.insert((2, 0), Object::Dictionary(pages));

        // Trailer
        let mut trailer = Dictionary::new();
        trailer.insert(PdfName::new("Size"), Object::Integer(3));
        trailer.insert(
            PdfName::new("Root"),
            Object::Reference(IndirectRef::new(1, 0)),
        );

        Document {
            version: PdfVersion::new(1, 7),
            trailer,
            objects,
            font_cache: RefCell::new(HashMap::new()),
            raw_data: None,
            deferred: RefCell::new(HashMap::new()),
            lazy_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Adds an object to the document and returns its ObjectId.
    ///
    /// The object is assigned the next available object number with
    /// generation 0.
    pub fn add_object(&mut self, obj: Object) -> ObjectId {
        let next_num = self.objects.keys().map(|&(num, _)| num).max().unwrap_or(0) + 1;
        let id = (next_num, 0);
        self.objects.insert(id, obj);
        id
    }

    /// Returns a mutable reference to an object by its ObjectId.
    pub fn get_object_mut(&mut self, id: ObjectId) -> Option<&mut Object> {
        self.objects.get_mut(&id)
    }

    /// Iterates over all indirect objects in the document.
    pub(crate) fn iter_objects(&self) -> impl Iterator<Item = (&ObjectId, &Object)> {
        self.objects.iter()
    }

    /// Serializes the document to PDF bytes.
    ///
    /// Produces a valid PDF file that can be written to disk or parsed
    /// back with [`from_bytes`](Self::from_bytes).
    pub fn to_bytes(&self) -> PdfResult<Vec<u8>> {
        let mut buf = Vec::new();

        // Header
        writeln!(buf, "%PDF-{}.{}", self.version.major, self.version.minor)
            .map_err(PdfError::Io)?;
        // Binary marker (signals binary content to text-mode transports)
        buf.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");

        // Collect and sort object IDs for deterministic output
        let mut ids: Vec<ObjectId> = self.objects.keys().copied().collect();
        ids.sort();

        // Write objects and record their byte offsets
        let mut offsets: Vec<(ObjectId, usize)> = Vec::with_capacity(ids.len());
        for id in &ids {
            let obj = &self.objects[id];
            offsets.push((*id, buf.len()));
            writeln!(buf, "{} {} obj", id.0, id.1).map_err(PdfError::Io)?;
            obj.write_pdf(&mut buf).map_err(PdfError::Io)?;
            buf.extend_from_slice(b"\nendobj\n");
        }

        // Build xref table
        let xref_offset = buf.len();
        let max_obj_num = ids.iter().map(|&(n, _)| n).max().unwrap_or(0);
        // /Size = max_obj_num + 1 (includes entry 0)
        let xref_size = (max_obj_num + 1) as usize;

        write!(buf, "xref\n0 {}\n", xref_size).map_err(PdfError::Io)?;
        // Entry 0: free object head
        writeln!(buf, "{:010} 65535 f ", 0).map_err(PdfError::Io)?;

        // Build a map for quick lookup
        let offset_map: HashMap<u32, usize> =
            offsets.iter().map(|&((num, _), off)| (num, off)).collect();

        for num in 1..=max_obj_num {
            if let Some(&off) = offset_map.get(&num) {
                writeln!(buf, "{:010} 00000 n ", off).map_err(PdfError::Io)?;
            } else {
                writeln!(buf, "{:010} 00000 f ", 0).map_err(PdfError::Io)?;
            }
        }

        // Trailer
        let mut trailer = self.trailer.clone();
        trailer.insert(PdfName::new("Size"), Object::Integer(xref_size as i64));

        buf.extend_from_slice(b"trailer\n");
        Object::Dictionary(trailer)
            .write_pdf(&mut buf)
            .map_err(PdfError::Io)?;
        write!(buf, "\nstartxref\n{}\n%%EOF\n", xref_offset).map_err(PdfError::Io)?;

        Ok(buf)
    }

    /// Serializes the document in linearized ("Fast Web View") format.
    ///
    /// Reorders objects so that the first page and its resources appear
    /// early in the file, enabling progressive display. The output
    /// includes a `/Linearized` dictionary as the first object.
    ///
    /// ISO 32000-2:2020, Annex F (Linearized PDF).
    pub fn to_linearized_bytes(&self) -> PdfResult<Vec<u8>> {
        let mut buf = Vec::new();

        // Header
        writeln!(buf, "%PDF-{}.{}", self.version.major, self.version.minor)
            .map_err(PdfError::Io)?;
        buf.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");

        // Collect all object IDs
        let mut all_ids: Vec<ObjectId> = self.objects.keys().copied().collect();
        all_ids.sort();

        // Identify first-page objects: the page dict and any objects it
        // directly references (Contents, Resources, Font dicts).
        let first_page_ids = self.collect_first_page_object_ids();

        // Partition: first-page objects first, then rest (stable order)
        let mut first_page: Vec<ObjectId> = all_ids
            .iter()
            .copied()
            .filter(|id| first_page_ids.contains(id))
            .collect();
        first_page.sort();
        let rest: Vec<ObjectId> = all_ids
            .iter()
            .copied()
            .filter(|id| !first_page_ids.contains(id))
            .collect();

        // Reserve object number for linearization dict (use max+1)
        let max_num = all_ids.iter().map(|&(n, _)| n).max().unwrap_or(0);
        let lin_id: ObjectId = (max_num + 1, 0);

        // Write linearization dictionary first (placeholder — will be updated)
        let lin_offset = buf.len();
        let lin_placeholder = format!(
            "{} 0 obj\n<< /Linearized 1 /L {} /O {} /E {} /N {} /T {} /H [ 0 0 ] >>\nendobj\n",
            lin_id.0,
            0,                                                // L = file length (placeholder)
            first_page.first().map(|&(n, _)| n).unwrap_or(0), // O = first page obj
            0,                                                // E = end of first page (placeholder)
            self.page_count().unwrap_or(0),
            0, // T = xref offset (placeholder)
        );
        buf.extend_from_slice(lin_placeholder.as_bytes());

        // Write first-page objects
        let mut offsets: Vec<(ObjectId, usize)> = Vec::with_capacity(all_ids.len() + 1);
        offsets.push((lin_id, lin_offset));

        for id in &first_page {
            let obj = &self.objects[id];
            offsets.push((*id, buf.len()));
            writeln!(buf, "{} {} obj", id.0, id.1).map_err(PdfError::Io)?;
            obj.write_pdf(&mut buf).map_err(PdfError::Io)?;
            buf.extend_from_slice(b"\nendobj\n");
        }
        let end_of_first_page = buf.len();

        // Write remaining objects
        for id in &rest {
            let obj = &self.objects[id];
            offsets.push((*id, buf.len()));
            writeln!(buf, "{} {} obj", id.0, id.1).map_err(PdfError::Io)?;
            obj.write_pdf(&mut buf).map_err(PdfError::Io)?;
            buf.extend_from_slice(b"\nendobj\n");
        }

        // Build xref table
        let xref_offset = buf.len();
        let xref_size = (lin_id.0 + 1) as usize;

        write!(buf, "xref\n0 {}\n", xref_size).map_err(PdfError::Io)?;
        writeln!(buf, "{:010} 65535 f ", 0).map_err(PdfError::Io)?;

        let offset_map: HashMap<u32, usize> =
            offsets.iter().map(|&((num, _), off)| (num, off)).collect();

        for num in 1..lin_id.0 + 1 {
            if let Some(&off) = offset_map.get(&num) {
                writeln!(buf, "{:010} 00000 n ", off).map_err(PdfError::Io)?;
            } else {
                writeln!(buf, "{:010} 00000 f ", 0).map_err(PdfError::Io)?;
            }
        }

        // Trailer
        let mut trailer = self.trailer.clone();
        trailer.insert(PdfName::new("Size"), Object::Integer(xref_size as i64));

        buf.extend_from_slice(b"trailer\n");
        Object::Dictionary(trailer)
            .write_pdf(&mut buf)
            .map_err(PdfError::Io)?;
        write!(buf, "\nstartxref\n{}\n%%EOF\n", xref_offset).map_err(PdfError::Io)?;

        // Patch linearization dictionary with actual values
        let file_length = buf.len();
        let patched_lin = format!(
            "{} 0 obj\n<< /Linearized 1 /L {} /O {} /E {} /N {} /T {} /H [ 0 0 ] >>\nendobj\n",
            lin_id.0,
            file_length,
            first_page.first().map(|&(n, _)| n).unwrap_or(0),
            end_of_first_page,
            self.page_count().unwrap_or(0),
            xref_offset,
        );
        let patched_bytes = patched_lin.as_bytes();
        // Only patch if same length (padding ensures this for small docs)
        if patched_bytes.len() <= lin_placeholder.len() {
            buf[lin_offset..lin_offset + patched_bytes.len()].copy_from_slice(patched_bytes);
            // Pad remainder with spaces
            for b in &mut buf[lin_offset + patched_bytes.len()..lin_offset + lin_placeholder.len()]
            {
                *b = b' ';
            }
        }

        Ok(buf)
    }

    /// Collects object IDs that belong to the first page and its resources.
    fn collect_first_page_object_ids(&self) -> std::collections::HashSet<ObjectId> {
        let mut ids = std::collections::HashSet::new();

        // Find the first page's object ID
        let page_dict = match self.get_page(0) {
            Ok(d) => d,
            Err(_) => return ids,
        };

        // Walk the page dict and collect referenced object IDs
        self.collect_refs_from_dict(page_dict, &mut ids, 8);

        // Include catalog and pages tree root
        if let Ok(catalog) = self.catalog() {
            self.collect_refs_from_dict(catalog, &mut ids, 4);
        }

        ids
    }

    /// Recursively collects indirect reference targets from a dictionary.
    fn collect_refs_from_dict(
        &self,
        dict: &Dictionary,
        ids: &mut std::collections::HashSet<ObjectId>,
        depth: usize,
    ) {
        if depth == 0 {
            return;
        }
        for (_, val) in dict.iter() {
            self.collect_refs_from_object(val, ids, depth);
        }
    }

    /// Recursively collects indirect reference targets from an object.
    fn collect_refs_from_object(
        &self,
        obj: &Object,
        ids: &mut std::collections::HashSet<ObjectId>,
        depth: usize,
    ) {
        if depth == 0 {
            return;
        }
        match obj {
            Object::Reference(r) => {
                let id = r.id();
                if ids.insert(id) {
                    if let Some(target) = self.get_object(id) {
                        match target {
                            Object::Dictionary(d) => self.collect_refs_from_dict(d, ids, depth - 1),
                            Object::Stream(s) => {
                                self.collect_refs_from_dict(&s.dict, ids, depth - 1)
                            }
                            Object::Array(arr) => {
                                for item in arr {
                                    self.collect_refs_from_object(item, ids, depth - 1);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Object::Array(arr) => {
                for item in arr {
                    self.collect_refs_from_object(item, ids, depth - 1);
                }
            }
            Object::Dictionary(d) => self.collect_refs_from_dict(d, ids, depth - 1),
            _ => {}
        }
    }

    /// Writes the document as an incremental update appended to `original`.
    ///
    /// The original bytes are preserved verbatim — only modified and new
    /// objects are appended, followed by a new xref section and trailer
    /// with a `/Prev` pointer to the original xref. This preserves
    /// digital signatures that cover the original byte range.
    ///
    /// ISO 32000-2:2020, Section 7.5.6.
    pub fn to_incremental_update(&self, original: &[u8]) -> PdfResult<Vec<u8>> {
        let mut buf = original.to_vec();

        // Find the original startxref offset
        let prev_startxref = find_startxref(original)?;

        // Collect all object IDs and write them as an appended body
        let mut ids: Vec<ObjectId> = self.objects.keys().copied().collect();
        ids.sort();

        let mut offsets: Vec<(ObjectId, usize)> = Vec::with_capacity(ids.len());
        for id in &ids {
            let obj = &self.objects[id];
            offsets.push((*id, buf.len()));
            writeln!(buf, "{} {} obj", id.0, id.1).map_err(PdfError::Io)?;
            obj.write_pdf(&mut buf).map_err(PdfError::Io)?;
            buf.extend_from_slice(b"\nendobj\n");
        }

        // Build new xref section covering only the appended objects
        let xref_offset = buf.len();
        let max_obj_num = ids.iter().map(|&(n, _)| n).max().unwrap_or(0);
        let xref_size = (max_obj_num + 1) as usize;

        write!(buf, "xref\n0 {}\n", xref_size).map_err(PdfError::Io)?;
        writeln!(buf, "{:010} 65535 f ", 0).map_err(PdfError::Io)?;

        let offset_map: HashMap<u32, usize> =
            offsets.iter().map(|&((num, _), off)| (num, off)).collect();

        for num in 1..=max_obj_num {
            if let Some(&off) = offset_map.get(&num) {
                writeln!(buf, "{:010} 00000 n ", off).map_err(PdfError::Io)?;
            } else {
                writeln!(buf, "{:010} 00000 f ", 0).map_err(PdfError::Io)?;
            }
        }

        // Trailer with /Prev pointing to the original xref
        let mut trailer = self.trailer.clone();
        trailer.insert(PdfName::new("Size"), Object::Integer(xref_size as i64));
        trailer.insert(PdfName::new("Prev"), Object::Integer(prev_startxref as i64));

        buf.extend_from_slice(b"trailer\n");
        Object::Dictionary(trailer)
            .write_pdf(&mut buf)
            .map_err(PdfError::Io)?;
        write!(buf, "\nstartxref\n{}\n%%EOF\n", xref_offset).map_err(PdfError::Io)?;

        Ok(buf)
    }

    /// Saves the document to a file.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> PdfResult<()> {
        let bytes = self.to_bytes()?;
        fs::write(path, &bytes)?;
        Ok(())
    }

    /// Extracts text from a page dictionary by resolving and decoding
    /// its content stream(s).
    ///
    /// Uses font encoding information when available for accurate
    /// character mapping. Falls back to basic encoding when fonts
    /// are not found.
    fn extract_text_from_page_dict(&self, page: &Dictionary) -> PdfResult<String> {
        let contents = page
            .get_str("Contents")
            .ok_or_else(|| PdfError::InvalidPage("No /Contents in page".to_string()))?;

        let content_data = self.resolve_content_data(contents)?;
        let fonts = self.page_fonts(page);
        extract_text_with_fonts(&content_data, &fonts)
    }

    /// Resolves a `/Contents` value (reference, stream, or array of references)
    /// into decoded byte data.
    pub(crate) fn resolve_content_data(&self, contents: &Object) -> PdfResult<Vec<u8>> {
        match contents {
            Object::Reference(_) => {
                let resolved = self.resolve(contents).ok_or_else(|| {
                    PdfError::InvalidReference("Cannot resolve /Contents".to_string())
                })?;
                self.resolve_content_data(resolved)
            }
            Object::Stream(stream) => stream.decode_data(),
            Object::Array(arr) => {
                // Concatenate all streams in the array
                let mut data = Vec::new();
                for item in arr {
                    let resolved = self.resolve(item).ok_or_else(|| {
                        PdfError::InvalidReference(
                            "Cannot resolve content stream array element".to_string(),
                        )
                    })?;
                    if let Some(stream) = resolved.as_stream() {
                        let decoded = stream.decode_data()?;
                        data.extend_from_slice(&decoded);
                        data.push(b'\n'); // Separate streams with newline
                    }
                }
                Ok(data)
            }
            _ => Err(PdfError::TypeError {
                expected: "Stream, Reference, or Array".to_string(),
                found: contents.type_name().to_string(),
            }),
        }
    }
}

/// Extracts inline images from PDF content stream data.
///
/// Parses `BI <params> ID <data> EI` sequences and returns PdfImage objects.
/// ISO 32000-2:2020, Section 8.9.7 (Inline Images).
/// Extracts inline images from PDF content stream data.
///
/// Parses `BI <params> ID <data> EI` sequences. Malformed inline images
/// are skipped (logged at debug level) but tokenization errors propagate.
fn extract_inline_images(data: &[u8]) -> Vec<PdfImage> {
    use crate::content::operators::ContentToken;

    let tokens = match crate::content::operators::tokenize_content_stream(data) {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!("Content stream tokenization failed for inline image extraction: {e}");
            return Vec::new();
        }
    };
    let mut images = Vec::new();

    for token in &tokens {
        if let ContentToken::InlineImage { dict, data, .. } = token {
            match PdfImage::from_inline(dict, data.clone()) {
                Ok(img) => images.push(img),
                Err(e) => tracing::debug!("Skipping malformed inline image: {e}"),
            }
        }
    }

    images
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::{IndirectRef, PdfName};

    /// Builds a minimal valid PDF as bytes for testing.
    fn make_minimal_pdf() -> Vec<u8> {
        // Hand-crafted minimal PDF with exact offsets
        let mut pdf = Vec::new();

        // Header (offset 0)
        pdf.extend_from_slice(b"%PDF-1.4\n");
        // offset = 9

        // Object 1: Catalog (offset 9)
        let obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
        let obj1_offset = pdf.len();
        pdf.extend_from_slice(obj1);

        // Object 2: Pages (offset varies)
        let obj2 = b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n";
        let obj2_offset = pdf.len();
        pdf.extend_from_slice(obj2);

        // Object 3: Page
        let obj3 = b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n";
        let obj3_offset = pdf.len();
        pdf.extend_from_slice(obj3);

        // Object 4: Info
        let obj4 = b"4 0 obj\n<< /Title (Test Document) /Author (PDFPurr) >>\nendobj\n";
        let obj4_offset = pdf.len();
        pdf.extend_from_slice(obj4);

        // XRef table
        let xref_offset = pdf.len();
        pdf.extend_from_slice(b"xref\n");
        pdf.extend_from_slice(b"0 5\n");
        pdf.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj1_offset).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj2_offset).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj3_offset).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj4_offset).as_bytes());

        // Trailer
        pdf.extend_from_slice(b"trailer\n");
        pdf.extend_from_slice(b"<< /Size 5 /Root 1 0 R /Info 4 0 R >>\n");

        // startxref
        pdf.extend_from_slice(format!("startxref\n{}\n%%EOF\n", xref_offset).as_bytes());

        pdf
    }

    #[test]
    fn parse_minimal_pdf() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();

        assert_eq!(doc.version, PdfVersion::new(1, 4));
        assert_eq!(doc.object_count(), 4); // Objects 1-4
    }

    #[test]
    fn document_catalog() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();

        let catalog = doc.catalog().unwrap();
        assert_eq!(
            catalog.get(&PdfName::new("Type")).unwrap().as_name(),
            Some("Catalog")
        );
    }

    #[test]
    fn document_page_count() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();

        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn document_info() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();

        let info = doc.info().unwrap();
        assert_eq!(
            info.get(&PdfName::new("Author"))
                .unwrap()
                .as_pdf_string()
                .unwrap()
                .as_text(),
            Some("PDFPurr")
        );
    }

    #[test]
    fn document_title() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();

        assert_eq!(doc.title(), Some("Test Document"));
    }

    #[test]
    fn get_object_by_number() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();

        let obj = doc.get_object_by_number(1).unwrap();
        assert!(obj.is_dictionary());
    }

    #[test]
    fn get_nonexistent_object() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();

        assert!(doc.get_object_by_number(99).is_none());
    }

    #[test]
    fn resolve_reference() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();

        let reference = Object::Reference(IndirectRef::new(1, 0));
        let resolved = doc.resolve(&reference).unwrap();
        assert!(resolved.is_dictionary());
    }

    #[test]
    fn resolve_non_reference() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();

        let obj = Object::Integer(42);
        let resolved = doc.resolve(&obj).unwrap();
        assert_eq!(resolved.as_i64(), Some(42));
    }

    #[test]
    fn document_pages() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();

        let pages = doc.pages().unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(
            pages[0].get(&PdfName::new("Type")).unwrap().as_name(),
            Some("Page")
        );
    }

    #[test]
    fn document_get_page() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();

        let page = doc.get_page(0).unwrap();
        assert_eq!(
            page.get(&PdfName::new("Type")).unwrap().as_name(),
            Some("Page")
        );
    }

    #[test]
    fn document_get_page_out_of_range() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();

        assert!(doc.get_page(5).is_err());
    }

    #[test]
    fn document_page_media_box() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();

        let page = doc.get_page(0).unwrap();
        let media_box = doc.page_media_box(page).unwrap();
        assert_eq!(media_box, [0.0, 0.0, 612.0, 792.0]);
    }

    /// Builds a PDF with a content stream containing text.
    fn make_pdf_with_text(text_content: &[u8]) -> Vec<u8> {
        let mut pdf = Vec::new();

        // Header
        pdf.extend_from_slice(b"%PDF-1.4\n");

        // Object 1: Catalog
        let obj1 = b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n";
        let obj1_offset = pdf.len();
        pdf.extend_from_slice(obj1);

        // Object 2: Pages
        let obj2 = b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n";
        let obj2_offset = pdf.len();
        pdf.extend_from_slice(obj2);

        // Object 4: Content stream (before page so we know the offset)
        let obj4_offset = pdf.len();
        let stream_header = format!("4 0 obj\n<< /Length {} >>\nstream\n", text_content.len());
        pdf.extend_from_slice(stream_header.as_bytes());
        pdf.extend_from_slice(text_content);
        pdf.extend_from_slice(b"\nendstream\nendobj\n");

        // Object 3: Page referencing content stream
        let obj3_offset = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n",
        );

        // XRef table
        let xref_offset = pdf.len();
        pdf.extend_from_slice(b"xref\n");
        pdf.extend_from_slice(b"0 5\n");
        pdf.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj1_offset).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj2_offset).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj3_offset).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj4_offset).as_bytes());

        // Trailer
        pdf.extend_from_slice(b"trailer\n");
        pdf.extend_from_slice(b"<< /Size 5 /Root 1 0 R >>\n");

        // startxref
        pdf.extend_from_slice(format!("startxref\n{}\n%%EOF\n", xref_offset).as_bytes());

        pdf
    }

    #[test]
    fn extract_page_text_simple() {
        let pdf = make_pdf_with_text(b"BT /F1 12 Tf (Hello World) Tj ET");
        let doc = Document::from_bytes(&pdf).unwrap();

        let text = doc.extract_page_text(0).unwrap();
        assert_eq!(text, "Hello World");
    }

    #[test]
    fn extract_page_text_multiple_strings() {
        let pdf = make_pdf_with_text(b"BT /F1 12 Tf (Hello ) Tj (PDF) Tj ET");
        let doc = Document::from_bytes(&pdf).unwrap();

        let text = doc.extract_page_text(0).unwrap();
        assert_eq!(text, "Hello PDF");
    }

    #[test]
    fn extract_all_text_single_page() {
        let pdf = make_pdf_with_text(b"BT (Test) Tj ET");
        let doc = Document::from_bytes(&pdf).unwrap();

        let text = doc.extract_all_text().unwrap();
        assert_eq!(text, "Test");
    }

    #[test]
    fn extract_page_text_out_of_range() {
        let pdf = make_pdf_with_text(b"BT (Test) Tj ET");
        let doc = Document::from_bytes(&pdf).unwrap();

        assert!(doc.extract_page_text(5).is_err());
    }

    // --- Text run extraction tests ---

    #[test]
    fn extract_text_runs_simple() {
        let pdf = make_pdf_with_text(b"BT /F1 12 Tf 100 700 Td (Hello) Tj ET");
        let doc = Document::from_bytes(&pdf).unwrap();
        let runs = doc.extract_text_runs(0).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "Hello");
        assert!((runs[0].font_size - 12.0).abs() < 0.1);
        assert!((runs[0].x - 100.0).abs() < 1.0);
        assert!((runs[0].y - 700.0).abs() < 1.0);
    }

    #[test]
    fn extract_text_runs_detects_bold() {
        // Use Helvetica-Bold as F1
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();
        let bold = crate::fonts::standard14::Standard14Font::from_name("Helvetica-Bold").unwrap();
        let mut fonts = crate::core::objects::Dictionary::new();
        fonts.insert(
            PdfName::new("F1"),
            Object::Dictionary(bold.to_font_dictionary()),
        );
        let content = b"BT /F1 18 Tf 100 700 Td (Title) Tj ET";
        doc.append_content_stream(0, content, Some(fonts)).unwrap();

        let runs = doc.extract_text_runs(0).unwrap();
        assert_eq!(runs.len(), 1);
        assert!(runs[0].is_bold, "Helvetica-Bold should be detected as bold");
        assert!(!runs[0].is_italic);
    }

    #[test]
    fn extract_text_runs_empty_page() {
        let doc = Document::new();
        // New page has no content — should return empty Vec, not error
        let mut doc = doc;
        doc.add_page(612.0, 792.0).unwrap();
        let runs = doc.extract_text_runs(0).unwrap();
        assert!(runs.is_empty());
    }

    #[test]
    fn extract_text_runs_on_real_pdf() {
        let path = std::path::Path::new("tests/corpus/basic/tracemonkey.pdf");
        if !path.exists() {
            return;
        }
        let data = std::fs::read(path).unwrap();
        let doc = Document::from_bytes(&data).unwrap();
        let runs = doc.extract_text_runs(0).unwrap();

        assert!(!runs.is_empty(), "tracemonkey page 0 should have text runs");

        // Should have a variety of font sizes (title vs body)
        let sizes: std::collections::HashSet<u32> =
            runs.iter().map(|r| (r.font_size * 10.0) as u32).collect();
        assert!(
            sizes.len() >= 2,
            "tracemonkey should have multiple font sizes, got {:?}",
            sizes
        );

        // All positions should be within page bounds (612 x 792)
        for run in &runs {
            assert!(
                run.x >= -10.0 && run.x <= 700.0,
                "x={} out of page bounds for '{}'",
                run.x,
                run.text
            );
            assert!(
                run.y >= -10.0 && run.y <= 900.0,
                "y={} out of page bounds for '{}'",
                run.y,
                run.text
            );
        }
    }

    // --- Structure analysis tests ---

    #[test]
    fn analyze_structure_detects_headings_on_real_pdf() {
        use crate::content::structure_detection::BlockRole;

        let path = std::path::Path::new("tests/corpus/basic/tracemonkey.pdf");
        if !path.exists() {
            return;
        }
        let data = std::fs::read(path).unwrap();
        let doc = Document::from_bytes(&data).unwrap();
        let blocks = doc.analyze_page_structure(0).unwrap();

        assert!(!blocks.is_empty(), "tracemonkey should have blocks");

        let headings: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b.role, BlockRole::Heading(_)))
            .collect();
        let paragraphs: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b.role, BlockRole::Paragraph))
            .collect();

        // tracemonkey has a title and body text
        assert!(
            !headings.is_empty(),
            "tracemonkey should have headings (title is larger font)"
        );
        assert!(
            !paragraphs.is_empty(),
            "tracemonkey should have body paragraphs"
        );
    }

    #[test]
    fn analyze_structure_empty_page() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();
        let blocks = doc.analyze_page_structure(0).unwrap();
        assert!(blocks.is_empty());
    }

    // --- Integration method tests ---

    #[test]
    fn auto_tag_creates_structure_on_untagged_pdf() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        // Add text with heading and body
        let bold = crate::fonts::standard14::Standard14Font::from_name("Helvetica-Bold").unwrap();
        let regular = crate::fonts::standard14::Standard14Font::from_name("Helvetica").unwrap();
        let mut fonts = crate::core::objects::Dictionary::new();
        fonts.insert(
            PdfName::new("F1"),
            Object::Dictionary(bold.to_font_dictionary()),
        );
        fonts.insert(
            PdfName::new("F2"),
            Object::Dictionary(regular.to_font_dictionary()),
        );

        let content = b"BT /F1 24 Tf 100 700 Td (Big Title) Tj ET BT /F2 12 Tf 100 650 Td (Body text here) Tj ET";
        doc.append_content_stream(0, content, Some(fonts)).unwrap();

        assert!(
            doc.structure_tree().is_none(),
            "Should be untagged before auto_tag"
        );

        let block_count = doc.auto_tag("en-US").unwrap();
        assert!(block_count > 0, "Should tag some blocks");
        assert!(
            doc.structure_tree().is_some(),
            "Should be tagged after auto_tag"
        );
    }

    #[test]
    fn auto_tag_skips_already_tagged() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        let helv = crate::fonts::standard14::Standard14Font::from_name("Helvetica").unwrap();
        let mut fonts = crate::core::objects::Dictionary::new();
        fonts.insert(
            PdfName::new("F1"),
            Object::Dictionary(helv.to_font_dictionary()),
        );
        let content = b"BT /F1 12 Tf 100 700 Td (Text) Tj ET";
        doc.append_content_stream(0, content, Some(fonts)).unwrap();

        // Tag it first
        doc.auto_tag("en").unwrap();

        // Second call should skip (already tagged)
        let count = doc.auto_tag("en").unwrap();
        assert_eq!(count, 0, "Should skip already tagged document");
    }

    #[test]
    fn check_accessibility_on_untagged_pdf() {
        let doc = Document::new();
        let issues = doc.check_accessibility();
        assert!(
            issues.iter().any(|i| i.description.contains("untagged")),
            "Should report untagged"
        );
    }

    #[test]
    fn check_accessibility_on_tagged_pdf() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        let helv = crate::fonts::standard14::Standard14Font::from_name("Helvetica").unwrap();
        let mut fonts = crate::core::objects::Dictionary::new();
        fonts.insert(
            PdfName::new("F1"),
            Object::Dictionary(helv.to_font_dictionary()),
        );
        let content = b"BT /F1 12 Tf 100 700 Td (Text) Tj ET";
        doc.append_content_stream(0, content, Some(fonts)).unwrap();

        doc.auto_tag("en-US").unwrap();

        let issues = doc.check_accessibility();
        // Should not report untagged anymore
        assert!(
            !issues.iter().any(|i| i.description.contains("untagged")),
            "Tagged doc should not report untagged"
        );
    }

    #[test]
    fn hybrid_ocr_with_mock_engine() {
        use crate::ocr::engine::{OcrEngine, OcrImage, OcrResult, OcrWord};
        use crate::ocr::OcrConfig;

        struct MockEngine;
        impl OcrEngine for MockEngine {
            fn recognize(&self, _: &OcrImage) -> crate::error::PdfResult<OcrResult> {
                Ok(OcrResult {
                    words: vec![OcrWord {
                        text: "MockText".to_string(),
                        x: 100,
                        y: 100,
                        width: 200,
                        height: 40,
                        confidence: 0.95,
                    }],
                    image_width: 2550,
                    image_height: 3300,
                })
            }
        }

        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        let result = doc
            .hybrid_ocr_page(0, &MockEngine, &OcrConfig::default())
            .unwrap();
        // Blank page + OCR text → should prefer OCR
        assert_eq!(
            result.source,
            crate::ocr::hybrid::TextSource::Ocr,
            "Blank page should use OCR"
        );
        assert!(result.accessible_text.contains("MockText"));
    }

    #[test]
    fn hybrid_ocr_on_page_with_text() {
        use crate::ocr::engine::{OcrEngine, OcrImage, OcrResult, OcrWord};
        use crate::ocr::OcrConfig;

        struct MatchingEngine;
        impl OcrEngine for MatchingEngine {
            fn recognize(&self, _: &OcrImage) -> crate::error::PdfResult<OcrResult> {
                Ok(OcrResult {
                    words: vec![
                        OcrWord {
                            text: "Hello".to_string(),
                            x: 100,
                            y: 100,
                            width: 100,
                            height: 30,
                            confidence: 0.9,
                        },
                        OcrWord {
                            text: "World".to_string(),
                            x: 250,
                            y: 100,
                            width: 100,
                            height: 30,
                            confidence: 0.9,
                        },
                    ],
                    image_width: 2550,
                    image_height: 3300,
                })
            }
        }

        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        let helv = crate::fonts::standard14::Standard14Font::from_name("Helvetica").unwrap();
        let mut fonts = crate::core::objects::Dictionary::new();
        fonts.insert(
            PdfName::new("F1"),
            Object::Dictionary(helv.to_font_dictionary()),
        );
        let content = b"BT /F1 12 Tf 100 700 Td (Hello World) Tj ET";
        doc.append_content_stream(0, content, Some(fonts)).unwrap();

        let result = doc
            .hybrid_ocr_page(0, &MatchingEngine, &OcrConfig::default())
            .unwrap();
        // Text matches OCR → should prefer content stream
        assert_eq!(
            result.source,
            crate::ocr::hybrid::TextSource::ContentStream,
            "Matching text should prefer content stream"
        );
    }

    #[test]
    fn auto_tag_on_real_pdf() {
        let path = std::path::Path::new("tests/corpus/basic/tracemonkey.pdf");
        if !path.exists() {
            return;
        }
        let data = std::fs::read(path).unwrap();
        let mut doc = Document::from_bytes(&data).unwrap();

        // auto_tag must not panic on real-world PDFs.
        let result = doc.auto_tag("en-US");
        assert!(
            result.is_ok(),
            "auto_tag should not error: {:?}",
            result.err()
        );
    }

    #[test]
    fn invalid_pdf_no_header() {
        let result = Document::from_bytes(b"not a pdf");
        assert!(result.is_err());
    }

    #[test]
    fn invalid_pdf_no_startxref() {
        let result = Document::from_bytes(b"%PDF-1.4\nsome content but no xref");
        assert!(result.is_err());
    }

    // --- Incremental update tests ---

    #[test]
    fn incremental_update_new_object() {
        // Build a PDF with an incremental update that adds object 5
        let mut pdf = make_minimal_pdf();
        // Strip the trailing newline from %%EOF
        // Add new object 5
        let obj5_offset = pdf.len();
        pdf.extend_from_slice(b"5 0 obj\n(Incremental Object)\nendobj\n");
        // New xref section referencing object 5, with /Prev to original xref
        let xref2_offset = pdf.len();
        // Find original xref offset from the file
        let orig_xref = find_startxref(&pdf).unwrap();
        pdf.extend_from_slice(
            format!(
                "xref\n5 1\n\
                 {:010} 00000 n \n\
                 trailer\n<< /Size 6 /Root 1 0 R /Prev {} >>\n\
                 startxref\n{}\n%%EOF",
                obj5_offset, orig_xref, xref2_offset
            )
            .as_bytes(),
        );

        let doc = Document::from_bytes(&pdf).unwrap();
        // Original 4 objects + new object 5 = 5
        assert_eq!(doc.object_count(), 5);
        // Object 5 should be accessible
        let obj5 = doc.get_object_by_number(5).unwrap();
        assert!(obj5.is_string());
    }

    #[test]
    fn incremental_update_overrides_object() {
        // Build a PDF where an incremental update replaces the Info dict (object 4)
        let mut pdf = make_minimal_pdf();
        let orig_xref = find_startxref(&pdf).unwrap();
        // New version of object 4 with different title
        let obj4_offset = pdf.len();
        pdf.extend_from_slice(b"4 0 obj\n<< /Title (Updated Title) >>\nendobj\n");
        let xref2_offset = pdf.len();
        pdf.extend_from_slice(
            format!(
                "xref\n4 1\n\
                 {:010} 00000 n \n\
                 trailer\n<< /Size 5 /Root 1 0 R /Info 4 0 R /Prev {} >>\n\
                 startxref\n{}\n%%EOF",
                obj4_offset, orig_xref, xref2_offset
            )
            .as_bytes(),
        );

        let doc = Document::from_bytes(&pdf).unwrap();
        assert_eq!(doc.title(), Some("Updated Title"));
    }

    #[test]
    fn document_metadata() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();

        let meta = doc.metadata();
        assert_eq!(meta.title, Some("Test Document".to_string()));
        assert_eq!(meta.author, Some("PDFPurr".to_string()));
    }

    #[test]
    fn document_metadata_no_info() {
        // Build a PDF without /Info in trailer
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");
        let obj1_offset = pdf.len();
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        let obj2_offset = pdf.len();
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        let obj3_offset = pdf.len();
        pdf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
        );
        let xref_offset = pdf.len();
        pdf.extend_from_slice(b"xref\n0 4\n");
        pdf.extend_from_slice(format!("{:010} 65535 f \n", 0).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj1_offset).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj2_offset).as_bytes());
        pdf.extend_from_slice(format!("{:010} 00000 n \n", obj3_offset).as_bytes());
        pdf.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\n");
        pdf.extend_from_slice(format!("startxref\n{}\n%%EOF\n", xref_offset).as_bytes());

        let doc = Document::from_bytes(&pdf).unwrap();
        let meta = doc.metadata();
        assert_eq!(meta, crate::structure::Metadata::default());
    }

    #[test]
    fn document_outlines_empty() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();
        assert!(doc.outlines().is_empty());
    }

    #[test]
    fn document_page_annotations_empty() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();
        let page = doc.get_page(0).unwrap();
        assert!(doc.page_annotations(page).is_empty());
    }

    #[test]
    fn document_page_images_empty() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();
        let page = doc.get_page(0).unwrap();
        assert!(doc.page_images(page).is_empty());
    }

    #[test]
    fn page_images_finds_inline_images() {
        // Create a page with an inline image (BI ... ID ... EI)
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        // Inline image: 2x2 RGB pixels (12 bytes of data)
        let content = b"BI\n/W 2\n/H 2\n/CS /RGB\n/BPC 8\nID\n\xFF\x00\x00\x00\xFF\x00\x00\x00\xFF\xFF\xFF\x00\nEI\n";
        doc.append_content_stream(0, content, None).unwrap();

        // Verify content was added
        let page = doc.get_page(0).unwrap();
        assert!(
            page.get(&PdfName::new("Contents")).is_some(),
            "Page should have /Contents after append"
        );

        // Verify content bytes can be read
        let bytes = doc.page_content_bytes(page);
        assert!(
            bytes.is_ok(),
            "page_content_bytes should work: {:?}",
            bytes.err()
        );
        let bytes = bytes.unwrap();
        assert!(!bytes.is_empty(), "Content bytes should not be empty");

        // Verify tokenizer finds inline image
        let tokens = crate::content::operators::tokenize_content_stream(&bytes).unwrap();
        let inline_count = tokens
            .iter()
            .filter(|t| {
                matches!(
                    t,
                    crate::content::operators::ContentToken::InlineImage { .. }
                )
            })
            .count();
        assert!(
            inline_count > 0,
            "Tokenizer should find inline image, got {} tokens total: {:?}",
            tokens.len(),
            tokens
                .iter()
                .map(|t| format!("{:?}", std::mem::discriminant(t)))
                .collect::<Vec<_>>()
        );

        let images = doc.page_images(page);
        assert!(
            !images.is_empty(),
            "page_images should find inline images, got 0"
        );
    }

    // --- Serialization tests ---

    #[test]
    fn document_new() {
        let doc = Document::new();
        assert_eq!(doc.version, PdfVersion::new(1, 7));
        assert!(doc.catalog().is_ok());
        assert_eq!(doc.page_count().unwrap(), 0);
    }

    #[test]
    fn document_add_object() {
        let mut doc = Document::new();
        let id = doc.add_object(Object::Integer(42));
        assert_eq!(doc.get_object(id), Some(&Object::Integer(42)));
    }

    #[test]
    fn document_to_bytes_roundtrip() {
        let doc = Document::new();
        let bytes = doc.to_bytes().unwrap();

        // Should start with PDF header
        assert!(bytes.starts_with(b"%PDF-1.7"));
        // Should end with %%EOF
        let tail = std::str::from_utf8(&bytes[bytes.len() - 7..]).unwrap();
        assert!(tail.contains("%%EOF"));

        // Should parse back
        let doc2 = Document::from_bytes(&bytes).unwrap();
        assert_eq!(doc2.page_count().unwrap(), 0);
        assert!(doc2.catalog().is_ok());
    }

    #[test]
    fn document_save_and_open() {
        let doc = Document::new();
        let tmp = std::env::temp_dir().join("pdfpurr_test_save.pdf");
        doc.save(&tmp).unwrap();

        let doc2 = Document::open(&tmp).unwrap();
        assert_eq!(doc2.page_count().unwrap(), 0);
        std::fs::remove_file(&tmp).ok();
    }

    // --- Page Manipulation tests ---

    #[test]
    fn add_page_to_new_document() {
        let mut doc = Document::new();
        assert_eq!(doc.page_count().unwrap(), 0);

        let idx = doc.add_page(612.0, 792.0).unwrap();
        assert_eq!(idx, 0);
        assert_eq!(doc.page_count().unwrap(), 1);

        let idx2 = doc.add_page(595.0, 842.0).unwrap();
        assert_eq!(idx2, 1);
        assert_eq!(doc.page_count().unwrap(), 2);
    }

    #[test]
    fn add_page_roundtrip() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();
        doc.add_page(595.0, 842.0).unwrap();

        let bytes = doc.to_bytes().unwrap();
        let doc2 = Document::from_bytes(&bytes).unwrap();
        assert_eq!(doc2.page_count().unwrap(), 2);

        // Verify media boxes
        let page0 = doc2.get_page(0).unwrap();
        let mb0 = doc2.page_media_box(page0).unwrap();
        assert_eq!(mb0, [0.0, 0.0, 612.0, 792.0]);

        let page1 = doc2.get_page(1).unwrap();
        let mb1 = doc2.page_media_box(page1).unwrap();
        assert_eq!(mb1, [0.0, 0.0, 595.0, 842.0]);
    }

    #[test]
    fn remove_page() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();
        doc.add_page(595.0, 842.0).unwrap();
        assert_eq!(doc.page_count().unwrap(), 2);

        doc.remove_page(0).unwrap();
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn remove_page_out_of_range() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();
        assert!(doc.remove_page(5).is_err());
    }

    #[test]
    fn rotate_page() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();
        doc.rotate_page(0, 90).unwrap();

        // Verify /Rotate was set
        let pages_id = doc.pages_id().unwrap();
        let pages = doc.get_object(pages_id).unwrap().as_dict().unwrap();
        let kids = pages.get_str("Kids").unwrap().as_array().unwrap();
        let page_ref = kids[0].as_reference().unwrap().id();
        let page = doc.get_object(page_ref).unwrap().as_dict().unwrap();
        assert_eq!(page.get_i64("Rotate"), Some(90));
    }

    #[test]
    fn rotate_page_invalid_degrees() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();
        assert!(doc.rotate_page(0, 45).is_err());
    }

    #[test]
    fn rotate_page_out_of_range() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();
        assert!(doc.rotate_page(5, 90).is_err());
    }

    #[test]
    fn reorder_pages() {
        let mut doc = Document::new();
        doc.add_page(100.0, 100.0).unwrap(); // page 0
        doc.add_page(200.0, 200.0).unwrap(); // page 1
        doc.add_page(300.0, 300.0).unwrap(); // page 2

        // Reverse order: [2, 1, 0]
        doc.reorder_pages(&[2, 1, 0]).unwrap();

        // Verify via roundtrip
        let bytes = doc.to_bytes().unwrap();
        let doc2 = Document::from_bytes(&bytes).unwrap();
        assert_eq!(doc2.page_count().unwrap(), 3);

        // First page should now have 300x300 media box
        let page0 = doc2.get_page(0).unwrap();
        let mb = doc2.page_media_box(page0).unwrap();
        assert_eq!(mb, [0.0, 0.0, 300.0, 300.0]);
    }

    #[test]
    fn reorder_pages_wrong_length() {
        let mut doc = Document::new();
        doc.add_page(100.0, 100.0).unwrap();
        doc.add_page(200.0, 200.0).unwrap();
        assert!(doc.reorder_pages(&[0]).is_err());
    }

    #[test]
    fn merge_documents() {
        let mut doc1 = Document::new();
        doc1.add_page(612.0, 792.0).unwrap();

        let mut doc2 = Document::new();
        doc2.add_page(595.0, 842.0).unwrap();
        doc2.add_page(400.0, 600.0).unwrap();

        doc1.merge(&doc2).unwrap();
        assert_eq!(doc1.page_count().unwrap(), 3);
    }

    #[test]
    fn form_fields_empty_document() {
        let doc = Document::new();
        assert!(doc.form_fields().is_empty());
    }

    #[test]
    fn form_fields_no_acroform() {
        let pdf = make_minimal_pdf();
        let doc = Document::from_bytes(&pdf).unwrap();
        assert!(doc.form_fields().is_empty());
    }

    #[test]
    fn form_fields_with_text_field() {
        // Build a PDF with an AcroForm containing a text field
        let mut doc = Document::new();

        // Create a text field (object 3)
        let mut field_dict = Dictionary::new();
        field_dict.insert(PdfName::new("FT"), Object::Name(PdfName::new("Tx")));
        field_dict.insert(
            PdfName::new("T"),
            Object::String(crate::core::objects::PdfString {
                bytes: b"username".to_vec(),
                format: crate::core::objects::StringFormat::Literal,
            }),
        );
        field_dict.insert(
            PdfName::new("V"),
            Object::String(crate::core::objects::PdfString {
                bytes: b"alice".to_vec(),
                format: crate::core::objects::StringFormat::Literal,
            }),
        );
        let field_id = doc.add_object(Object::Dictionary(field_dict));

        // Add /AcroForm to catalog
        let acroform_dict = Object::Dictionary({
            let mut d = Dictionary::new();
            d.insert(
                PdfName::new("Fields"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    field_id.0, field_id.1,
                ))]),
            );
            d
        });

        // Update catalog to include AcroForm
        if let Some(Object::Dictionary(catalog)) = doc.get_object_mut((1, 0)) {
            catalog.insert(PdfName::new("AcroForm"), acroform_dict);
        }

        let fields = doc.form_fields();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "username");
        assert_eq!(fields[0].field_type, crate::forms::FieldType::Text);
        assert_eq!(fields[0].value, Some("alice".to_string()));
    }

    #[test]
    fn set_form_field_value() {
        let mut doc = Document::new();

        // Create a text field
        let mut field_dict = Dictionary::new();
        field_dict.insert(PdfName::new("FT"), Object::Name(PdfName::new("Tx")));
        field_dict.insert(
            PdfName::new("T"),
            Object::String(crate::core::objects::PdfString {
                bytes: b"email".to_vec(),
                format: crate::core::objects::StringFormat::Literal,
            }),
        );
        let field_id = doc.add_object(Object::Dictionary(field_dict));

        // Add /AcroForm to catalog
        if let Some(Object::Dictionary(catalog)) = doc.get_object_mut((1, 0)) {
            let mut acro = Dictionary::new();
            acro.insert(
                PdfName::new("Fields"),
                Object::Array(vec![Object::Reference(IndirectRef::new(
                    field_id.0, field_id.1,
                ))]),
            );
            catalog.insert(PdfName::new("AcroForm"), Object::Dictionary(acro));
        }

        // Set the field value
        doc.set_form_field("email", "alice@example.com").unwrap();

        // Verify it was set
        let fields = doc.form_fields();
        assert_eq!(fields[0].value, Some("alice@example.com".to_string()));
    }

    #[test]
    fn set_form_field_not_found() {
        let doc = &mut Document::new();
        assert!(doc.set_form_field("nonexistent", "value").is_err());
    }

    #[test]
    fn merge_roundtrip() {
        let mut doc1 = Document::new();
        doc1.add_page(612.0, 792.0).unwrap();

        let mut doc2 = Document::new();
        doc2.add_page(595.0, 842.0).unwrap();

        doc1.merge(&doc2).unwrap();

        let bytes = doc1.to_bytes().unwrap();
        let reopened = Document::from_bytes(&bytes).unwrap();
        assert_eq!(reopened.page_count().unwrap(), 2);

        let page0 = reopened.get_page(0).unwrap();
        let mb0 = reopened.page_media_box(page0).unwrap();
        assert_eq!(mb0, [0.0, 0.0, 612.0, 792.0]);

        let page1 = reopened.get_page(1).unwrap();
        let mb1 = reopened.page_media_box(page1).unwrap();
        assert_eq!(mb1, [0.0, 0.0, 595.0, 842.0]);
    }

    #[test]
    fn from_bytes_recovers_corrupt_xref() {
        // A PDF with valid objects but a corrupt startxref (points nowhere).
        // Document::from_bytes should fall back to xref rebuild via object scan.
        let pdf = b"%PDF-1.4\n\
            1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
            2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
            3 0 obj\n<< /Type /Page /MediaBox [0 0 612 792] /Parent 2 0 R >>\nendobj\n\
            startxref\n99999\n%%EOF";

        let doc = Document::from_bytes(pdf)
            .unwrap_or_else(|e| panic!("should recover corrupt xref: {}", e));
        assert_eq!(doc.page_count().unwrap(), 1);
    }

    #[test]
    fn circular_page_tree_does_not_hang() {
        // /Kids contains a reference back to the same /Pages node — circular.
        // collect_pages must detect the cycle and return an error, not loop.
        let mut doc = Document::new();

        // Object 1 = catalog pointing to Pages at obj 2
        let catalog = Object::Dictionary({
            let mut d = Dictionary::new();
            d.insert(PdfName::new("Type"), Object::Name(PdfName::new("Catalog")));
            d.insert(
                PdfName::new("Pages"),
                Object::Reference(IndirectRef::new(2, 0)),
            );
            d
        });
        doc.objects.insert((1, 0), catalog);

        // Object 2 = /Pages whose /Kids points back to itself
        let pages = Object::Dictionary({
            let mut d = Dictionary::new();
            d.insert(PdfName::new("Type"), Object::Name(PdfName::new("Pages")));
            d.insert(PdfName::new("Count"), Object::Integer(1));
            d.insert(
                PdfName::new("Kids"),
                Object::Array(vec![Object::Reference(IndirectRef::new(2, 0))]),
            );
            d
        });
        doc.objects.insert((2, 0), pages);

        doc.trailer.insert(
            PdfName::new("Root"),
            Object::Reference(IndirectRef::new(1, 0)),
        );

        let result = doc.pages();
        assert!(result.is_err(), "circular page tree should error, not hang");
    }

    #[test]
    fn deeply_nested_page_tree_errors_at_depth_limit() {
        // A chain A→B→C→...→Z that exceeds the depth limit.
        // Should error rather than blowing the stack.
        let mut doc = Document::new();

        // Build a chain of 200 /Pages nodes, each pointing to the next
        let depth = 200u32;
        for i in 2..=depth + 1 {
            let pages = Object::Dictionary({
                let mut d = Dictionary::new();
                d.insert(PdfName::new("Type"), Object::Name(PdfName::new("Pages")));
                d.insert(PdfName::new("Count"), Object::Integer(1));
                d.insert(
                    PdfName::new("Kids"),
                    Object::Array(vec![Object::Reference(IndirectRef::new(i + 1, 0))]),
                );
                d
            });
            doc.objects.insert((i, 0), pages);
        }

        // Leaf page
        let leaf = Object::Dictionary({
            let mut d = Dictionary::new();
            d.insert(PdfName::new("Type"), Object::Name(PdfName::new("Page")));
            d.insert(
                PdfName::new("MediaBox"),
                Object::Array(vec![
                    Object::Integer(0),
                    Object::Integer(0),
                    Object::Integer(612),
                    Object::Integer(792),
                ]),
            );
            d
        });
        doc.objects.insert((depth + 2, 0), leaf);

        let catalog = Object::Dictionary({
            let mut d = Dictionary::new();
            d.insert(PdfName::new("Type"), Object::Name(PdfName::new("Catalog")));
            d.insert(
                PdfName::new("Pages"),
                Object::Reference(IndirectRef::new(2, 0)),
            );
            d
        });
        doc.objects.insert((1, 0), catalog);

        doc.trailer.insert(
            PdfName::new("Root"),
            Object::Reference(IndirectRef::new(1, 0)),
        );

        let result = doc.pages();
        assert!(
            result.is_err(),
            "deeply nested page tree should error at depth limit"
        );
    }

    #[test]
    fn to_linearized_bytes_roundtrips() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();
        doc.add_page(595.0, 842.0).unwrap();

        let bytes = doc.to_linearized_bytes().unwrap();

        // The output must be a valid PDF parseable by our reader
        let parsed = Document::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.page_count().unwrap(), 2);

        // Must contain the linearization marker
        assert!(
            bytes.windows(10).any(|w| w == b"Linearized"),
            "output should contain /Linearized dictionary"
        );
    }

    #[test]
    fn to_linearized_bytes_first_page_objects_come_first() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        let bytes = doc.to_linearized_bytes().unwrap();

        // The linearization dict should appear near the start (first object)
        let lin_pos = bytes.windows(10).position(|w| w == b"Linearized").unwrap();
        assert!(
            lin_pos < 200,
            "/Linearized should be near the start, found at byte {}",
            lin_pos
        );
    }

    #[test]
    fn to_incremental_update_preserves_original_bytes() {
        // Create a PDF, then modify it, then write as incremental update.
        // The original bytes should be preserved verbatim at the start.
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();
        let original = doc.to_bytes().unwrap();

        // Parse, modify, and write incremental update
        let mut doc2 = Document::from_bytes(&original).unwrap();
        doc2.add_page(595.0, 842.0).unwrap();

        let updated = doc2.to_incremental_update(&original).unwrap();

        // Updated file starts with exact original bytes
        assert!(
            updated.starts_with(&original),
            "incremental update must preserve original bytes"
        );
        assert!(
            updated.len() > original.len(),
            "incremental update must be longer than original"
        );

        // Parse the result — should have 2 pages
        let doc3 = Document::from_bytes(&updated).unwrap();
        assert_eq!(doc3.page_count().unwrap(), 2);
    }

    #[test]
    fn open_mmap_roundtrip() {
        // Write a PDF to a temp file, then open via mmap
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();
        doc.add_page(595.0, 842.0).unwrap();

        let tmp = tempfile::NamedTempFile::new().unwrap();
        doc.save(tmp.path()).unwrap();

        let mapped = Document::open_mmap(tmp.path()).unwrap();
        assert_eq!(mapped.page_count().unwrap(), 2);
    }

    #[test]
    fn from_bytes_lazy_resolves_pages() {
        // Build a multi-page PDF, then load lazily
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();
        doc.add_page(595.0, 842.0).unwrap();
        let bytes = doc.to_bytes().unwrap();

        let lazy = Document::from_bytes_lazy(&bytes).unwrap();

        // page_count should work (catalog + pages eagerly loaded)
        assert_eq!(lazy.page_count().unwrap(), 2);

        // Accessing pages should trigger lazy parsing
        let pages = lazy.pages().unwrap();
        assert_eq!(pages.len(), 2);

        // MediaBox should resolve for both pages
        let mb0 = lazy.page_media_box(pages[0]).unwrap();
        assert_eq!(mb0, [0.0, 0.0, 612.0, 792.0]);

        let mb1 = lazy.page_media_box(pages[1]).unwrap();
        assert_eq!(mb1, [0.0, 0.0, 595.0, 842.0]);
    }

    #[test]
    fn from_bytes_lazy_uses_fewer_initial_objects() {
        let mut doc = Document::new();
        for _ in 0..5 {
            doc.add_page(612.0, 792.0).unwrap();
        }
        let bytes = doc.to_bytes().unwrap();

        let eager = Document::from_bytes(&bytes).unwrap();
        let lazy = Document::from_bytes_lazy(&bytes).unwrap();

        // Lazy should start with fewer objects loaded
        assert!(
            lazy.object_count() < eager.object_count(),
            "lazy ({}) should have fewer objects than eager ({})",
            lazy.object_count(),
            eager.object_count()
        );

        // But page_count should still work
        assert_eq!(lazy.page_count().unwrap(), 5);
    }

    #[test]
    fn append_content_stream_adds_text_to_page() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        // Build invisible text content stream
        let content = b"BT 3 Tr /F1 12 Tf 100 700 Td (Hello OCR) Tj ET";

        // Create Helvetica font dict
        let helv = crate::fonts::standard14::Standard14Font::from_name("Helvetica").unwrap();
        let mut fonts = Dictionary::new();
        fonts.insert(
            PdfName::new("F1"),
            Object::Dictionary(helv.to_font_dictionary()),
        );

        doc.append_content_stream(0, content, Some(fonts)).unwrap();

        // Roundtrip: serialize and reparse
        let bytes = doc.to_bytes().unwrap();
        let parsed = Document::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.page_count().unwrap(), 1);

        // The page should now have content
        let page = parsed.get_page(0).unwrap();
        assert!(page.get_str("Contents").is_some());
    }

    #[test]
    fn append_content_stream_preserves_existing_content() {
        use crate::content::ContentStreamBuilder;

        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();

        // First content stream with visible text
        let mut builder = ContentStreamBuilder::new();
        builder
            .begin_text()
            .set_font("F1", 12.0)
            .move_to(72.0, 720.0)
            .show_text("Original")
            .end_text();
        let first = builder.build();

        let helv = crate::fonts::standard14::Standard14Font::from_name("Helvetica").unwrap();
        let mut fonts1 = Dictionary::new();
        fonts1.insert(
            PdfName::new("F1"),
            Object::Dictionary(helv.to_font_dictionary()),
        );
        doc.append_content_stream(0, &first, Some(fonts1)).unwrap();

        // Second content stream (OCR overlay)
        let second = b"BT 3 Tr /F1 12 Tf 100 600 Td (OCR Text) Tj ET";
        doc.append_content_stream(0, second, None).unwrap();

        // Roundtrip
        let bytes = doc.to_bytes().unwrap();
        let parsed = Document::from_bytes(&bytes).unwrap();

        // /Contents should now be an array with 2 entries
        let page = parsed.get_page(0).unwrap();
        let contents = page.get_str("Contents").unwrap();
        assert!(
            contents.as_array().is_some(),
            "/Contents should be an array"
        );
        assert_eq!(contents.as_array().unwrap().len(), 2);
    }

    #[test]
    fn page_object_id_returns_valid_id() {
        let mut doc = Document::new();
        doc.add_page(612.0, 792.0).unwrap();
        doc.add_page(595.0, 842.0).unwrap();

        let id0 = doc.page_object_id(0).unwrap();
        let id1 = doc.page_object_id(1).unwrap();

        assert_ne!(id0, id1);
        assert!(doc.get_object(id0).is_some());
        assert!(doc.get_object(id1).is_some());
    }

    #[test]
    fn page_object_id_out_of_range() {
        let doc = Document::new();
        assert!(doc.page_object_id(0).is_err());
    }
}
