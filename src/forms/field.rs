//! PDF form field types and parsing.

use crate::core::objects::{DictExt, Dictionary, Object, ObjectId};

/// The type of a PDF form field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// Text input field (`/FT /Tx`).
    Text,
    /// Button field — checkbox or radio button (`/FT /Btn`).
    Button,
    /// Choice field — dropdown or list box (`/FT /Ch`).
    Choice,
    /// Digital signature field (`/FT /Sig`).
    Signature,
}

impl FieldType {
    /// Parses a field type from the `/FT` name value.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "Tx" => Some(FieldType::Text),
            "Btn" => Some(FieldType::Button),
            "Ch" => Some(FieldType::Choice),
            "Sig" => Some(FieldType::Signature),
            _ => None,
        }
    }
}

/// A parsed PDF form field.
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    /// Fully qualified field name (e.g. `"person.name.first"`).
    pub name: String,
    /// The field type.
    pub field_type: FieldType,
    /// Current value (`/V`).
    pub value: Option<String>,
    /// Default value (`/DV`).
    pub default_value: Option<String>,
    /// Field flags (`/Ff`).
    pub flags: u32,
    /// Options for choice fields (`/Opt`).
    pub options: Vec<String>,
    /// The object ID of this field's dictionary.
    pub obj_id: ObjectId,
}

/// Extracts the string value from a `/V` or `/DV` entry.
fn extract_field_value(obj: &Object) -> Option<String> {
    obj.as_text_string()
        .or_else(|| obj.as_name().map(|n| n.to_string()))
}

/// Extracts options from an `/Opt` array.
fn extract_options(obj: &Object) -> Vec<String> {
    match obj {
        Object::Array(arr) => arr
            .iter()
            .filter_map(|item| {
                item.as_text_string().or_else(|| {
                    // /Opt can contain [export_value, display_value] pairs
                    item.as_array()
                        .and_then(|pair| pair.last())
                        .and_then(|o| o.as_text_string())
                })
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Collects form fields from an AcroForm fields tree.
///
/// Walks the `/Fields` array recursively (fields can have `/Kids`
/// sub-fields), building fully qualified names by joining `/T` values
/// with `.`.
///
/// The `resolve` closure resolves indirect references to their objects.
pub fn collect_fields<'a, F>(fields: &'a [Object], parent_name: &str, resolve: &F) -> Vec<FormField>
where
    F: Fn(&'a Object) -> Option<&'a Object>,
{
    collect_fields_inner(fields, parent_name, resolve, 32)
}

fn collect_fields_inner<'a, F>(
    fields: &'a [Object],
    parent_name: &str,
    resolve: &F,
    depth: usize,
) -> Vec<FormField>
where
    F: Fn(&'a Object) -> Option<&'a Object>,
{
    if depth == 0 {
        return Vec::new();
    }

    let mut result = Vec::new();

    for field_obj in fields {
        let (dict, obj_id) = match field_obj {
            Object::Reference(r) => {
                let id = r.id();
                match resolve(field_obj).and_then(|o| o.as_dict()) {
                    Some(d) => (d, id),
                    None => continue,
                }
            }
            Object::Dictionary(d) => (d, (0, 0)),
            _ => continue,
        };

        // Build qualified name
        let partial_name = dict.get_text("T");

        let qualified_name = match &partial_name {
            Some(t) if !parent_name.is_empty() => format!("{}.{}", parent_name, t),
            Some(t) => t.clone(),
            None => parent_name.to_string(),
        };

        // Check for /Kids (non-terminal node)
        if let Some(kids) = dict.get_str("Kids").and_then(|o| o.as_array()) {
            // If kids have /T entries, they're sub-fields; recurse
            let sub_fields = collect_fields_inner(kids, &qualified_name, resolve, depth - 1);
            if !sub_fields.is_empty() {
                result.extend(sub_fields);
                continue;
            }
            // Otherwise kids are widget annotations — treat this as a leaf
        }

        // Extract field type — may be inherited from parent, but for
        // synthetic construction we require it on the field itself
        let field_type = match get_field_type(dict) {
            Some(ft) => ft,
            None => continue, // Skip fields without a type
        };

        let value = dict.get_str("V").and_then(extract_field_value);
        let default_value = dict.get_str("DV").and_then(extract_field_value);
        let flags = dict.get_i64("Ff").unwrap_or(0) as u32;
        let options = dict.get_str("Opt").map(extract_options).unwrap_or_default();

        result.push(FormField {
            name: qualified_name,
            field_type,
            value,
            default_value,
            flags,
            options,
            obj_id,
        });
    }

    result
}

/// Gets the field type from a dictionary's `/FT` entry.
fn get_field_type(dict: &Dictionary) -> Option<FieldType> {
    dict.get_name("FT").and_then(FieldType::from_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::objects::{PdfName, PdfString, StringFormat};
    use crate::test_utils::{make_dict, str_obj};

    fn no_resolve<'a>(obj: &'a Object) -> Option<&'a Object> {
        Some(obj)
    }

    #[test]
    fn field_type_from_name() {
        assert_eq!(FieldType::from_name("Tx"), Some(FieldType::Text));
        assert_eq!(FieldType::from_name("Btn"), Some(FieldType::Button));
        assert_eq!(FieldType::from_name("Ch"), Some(FieldType::Choice));
        assert_eq!(FieldType::from_name("Sig"), Some(FieldType::Signature));
        assert_eq!(FieldType::from_name("Unknown"), None);
    }

    #[test]
    fn parse_text_field() {
        let dict = make_dict(vec![
            ("FT", Object::Name(PdfName::new("Tx"))),
            ("T", str_obj("first_name")),
            ("V", str_obj("Alice")),
        ]);

        let fields = collect_fields(&[Object::Dictionary(dict)], "", &no_resolve);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "first_name");
        assert_eq!(fields[0].field_type, FieldType::Text);
        assert_eq!(fields[0].value, Some("Alice".to_string()));
    }

    #[test]
    fn parse_checkbox_field() {
        let dict = make_dict(vec![
            ("FT", Object::Name(PdfName::new("Btn"))),
            ("T", str_obj("agree")),
            ("V", Object::Name(PdfName::new("Yes"))),
        ]);

        let fields = collect_fields(&[Object::Dictionary(dict)], "", &no_resolve);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_type, FieldType::Button);
        assert_eq!(fields[0].value, Some("Yes".to_string()));
    }

    #[test]
    fn parse_choice_field_with_options() {
        let dict = make_dict(vec![
            ("FT", Object::Name(PdfName::new("Ch"))),
            ("T", str_obj("country")),
            (
                "Opt",
                Object::Array(vec![str_obj("USA"), str_obj("Canada"), str_obj("Mexico")]),
            ),
            ("V", str_obj("USA")),
        ]);

        let fields = collect_fields(&[Object::Dictionary(dict)], "", &no_resolve);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_type, FieldType::Choice);
        assert_eq!(fields[0].options, vec!["USA", "Canada", "Mexico"]);
        assert_eq!(fields[0].value, Some("USA".to_string()));
    }

    #[test]
    fn qualified_names_with_parent() {
        let child = make_dict(vec![
            ("FT", Object::Name(PdfName::new("Tx"))),
            ("T", str_obj("name")),
        ]);

        let parent = make_dict(vec![
            ("T", str_obj("person")),
            ("Kids", Object::Array(vec![Object::Dictionary(child)])),
        ]);

        let fields = collect_fields(&[Object::Dictionary(parent)], "", &no_resolve);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "person.name");
    }

    #[test]
    fn empty_fields_array() {
        let fields = collect_fields(&[], "", &no_resolve);
        assert!(fields.is_empty());
    }

    #[test]
    fn field_with_flags() {
        let dict = make_dict(vec![
            ("FT", Object::Name(PdfName::new("Tx"))),
            ("T", str_obj("readonly_field")),
            ("Ff", Object::Integer(1)),
        ]);

        let fields = collect_fields(&[Object::Dictionary(dict)], "", &no_resolve);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].flags, 1);
    }

    #[test]
    fn field_without_type_skipped() {
        let dict = make_dict(vec![("T", str_obj("no_type"))]);
        let fields = collect_fields(&[Object::Dictionary(dict)], "", &no_resolve);
        assert!(fields.is_empty());
    }

    #[test]
    fn field_default_value() {
        let dict = make_dict(vec![
            ("FT", Object::Name(PdfName::new("Tx"))),
            ("T", str_obj("with_default")),
            ("DV", str_obj("default_text")),
        ]);

        let fields = collect_fields(&[Object::Dictionary(dict)], "", &no_resolve);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].default_value, Some("default_text".to_string()));
        assert_eq!(fields[0].value, None);
    }

    #[test]
    fn signature_field_parsed() {
        let dict = make_dict(vec![
            ("FT", Object::Name(PdfName::new("Sig"))),
            ("T", str_obj("sig_field")),
        ]);

        let fields = collect_fields(&[Object::Dictionary(dict)], "", &no_resolve);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_type, FieldType::Signature);
        assert_eq!(fields[0].name, "sig_field");
    }

    #[test]
    fn multiple_fields_at_same_level() {
        let field1 = make_dict(vec![
            ("FT", Object::Name(PdfName::new("Tx"))),
            ("T", str_obj("name")),
        ]);
        let field2 = make_dict(vec![
            ("FT", Object::Name(PdfName::new("Btn"))),
            ("T", str_obj("submit")),
        ]);

        let fields = collect_fields(
            &[Object::Dictionary(field1), Object::Dictionary(field2)],
            "",
            &no_resolve,
        );
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "name");
        assert_eq!(fields[0].field_type, FieldType::Text);
        assert_eq!(fields[1].name, "submit");
        assert_eq!(fields[1].field_type, FieldType::Button);
    }

    #[test]
    fn deeply_nested_fields() {
        let child = make_dict(vec![
            ("FT", Object::Name(PdfName::new("Tx"))),
            ("T", str_obj("child")),
        ]);
        let parent = make_dict(vec![
            ("T", str_obj("parent")),
            ("Kids", Object::Array(vec![Object::Dictionary(child)])),
        ]);
        let grandparent = make_dict(vec![
            ("T", str_obj("grandparent")),
            ("Kids", Object::Array(vec![Object::Dictionary(parent)])),
        ]);

        let fields = collect_fields(&[Object::Dictionary(grandparent)], "", &no_resolve);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "grandparent.parent.child");
    }

    #[test]
    fn choice_field_with_export_display_pairs() {
        let dict = make_dict(vec![
            ("FT", Object::Name(PdfName::new("Ch"))),
            ("T", str_obj("lang")),
            (
                "Opt",
                Object::Array(vec![
                    Object::Array(vec![str_obj("en"), str_obj("English")]),
                    Object::Array(vec![str_obj("fr"), str_obj("French")]),
                ]),
            ),
        ]);

        let fields = collect_fields(&[Object::Dictionary(dict)], "", &no_resolve);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].options, vec!["English", "French"]);
    }

    #[test]
    fn non_dictionary_objects_skipped() {
        let fields = collect_fields(
            &[Object::Integer(42), Object::Boolean(true)],
            "",
            &no_resolve,
        );
        assert!(fields.is_empty());
    }

    #[test]
    fn depth_limit_prevents_infinite_recursion() {
        fn make_deep_field(depth: usize) -> Object {
            if depth == 0 {
                let dict = make_dict(vec![
                    ("FT", Object::Name(PdfName::new("Tx"))),
                    ("T", str_obj("leaf")),
                ]);
                return Object::Dictionary(dict);
            }
            let child = make_deep_field(depth - 1);
            let dict = make_dict(vec![
                (
                    "T",
                    Object::String(PdfString {
                        bytes: format!("level_{}", depth).into_bytes(),
                        format: StringFormat::Literal,
                    }),
                ),
                ("Kids", Object::Array(vec![child])),
            ]);
            Object::Dictionary(dict)
        }

        let deep = make_deep_field(40);
        let fields = collect_fields(&[deep], "", &no_resolve);
        assert!(fields.len() <= 1);
    }

    #[test]
    fn extract_field_value_from_name() {
        let obj = Object::Name(PdfName::new("Off"));
        assert_eq!(extract_field_value(&obj), Some("Off".to_string()));
    }

    #[test]
    fn extract_options_empty_on_non_array() {
        let obj = Object::Integer(42);
        assert!(extract_options(&obj).is_empty());
    }
}
