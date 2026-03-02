use serde::Serialize;
use thiserror::Error;
use std::collections::HashMap;

use crate::database::schema::Subscription;

#[derive(Error, Debug)]
pub enum OpmlExportError {
    #[error("XML serialization error: {0}")]
    XmlError(#[from] quick_xml::Error),
    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),
}

#[derive(Serialize)]
struct Opml {
    version: String,
    #[serde(rename = "head")]
    head: Head,
    #[serde(rename = "body")]
    body: Body,
}

#[derive(Serialize)]
struct Head {
    #[serde(rename = "title")]
    title: String,
    #[serde(rename = "dateCreated")]
    date_created: String,
}

#[derive(Serialize)]
struct Body {
    #[serde(rename = "outline")]
    outlines: Vec<Outline>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Outline {
    #[serde(rename = "text", skip_serializing_if = "Option::is_none")]
    text: Option<String>,

    #[serde(rename = "title", skip_serializing_if = "Option::is_none")]
    title: Option<String>,

    #[serde(rename = "xmlUrl", skip_serializing_if = "Option::is_none")]
    xml_url: Option<String>,

    #[serde(rename = "htmlUrl", skip_serializing_if = "Option::is_none")]
    html_url: Option<String>,

    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    outline_type: Option<String>,
}

pub fn export_opml(subscriptions: &[Subscription]) -> Result<String, OpmlExportError> {
    let outlines: Vec<Outline> = subscriptions
        .iter()
        .map(|sub| {
            let mut extended_attrs = HashMap::new();

            // Parse extended attributes from JSON if present
            if let Some(attrs_json) = &sub.opml_attributes {
                if let Ok(attrs) = serde_json::from_str::<HashMap<String, serde_json::Value>>(attrs_json) {
                    for (key, value) in attrs {
                        if let Some(str_val) = value.as_str() {
                            extended_attrs.insert(key, str_val.to_string());
                        }
                    }
                }
            }

            Outline {
                text: sub.title.clone(),
                title: sub.title.clone(),
                xml_url: Some(sub.url.clone()),
                html_url: sub.website_url.clone(),
                outline_type: Some("rss".to_string()),
            }
        })
        .collect();

    let _opml = Opml {
        version: "2.0".to_string(),
        head: Head {
            title: "RSS Reader Subscriptions".to_string(),
            date_created: chrono::Utc::now().to_rfc3339(),
        },
        body: Body { outlines },
    };

    // Use manual XML building to support extended attributes
    let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    xml.push_str(r#"<opml version="2.0">"#);
    xml.push_str("<head>");
    xml.push_str(&format!("<title>RSS Reader Subscriptions</title>"));
    xml.push_str(&format!("<dateCreated>{}</dateCreated>", chrono::Utc::now().to_rfc3339()));
    xml.push_str("</head>");
    xml.push_str("<body>");

    for sub in subscriptions {
        xml.push_str("<outline");
        xml.push_str(" type=\"rss\"");

        if let Some(title) = &sub.title {
            xml.push_str(&format!(" title=\"{}\"", escape_xml(title)));
        }

        xml.push_str(&format!(" xmlUrl=\"{}\"", escape_xml(&sub.url)));

        if let Some(website_url) = &sub.website_url {
            xml.push_str(&format!(" htmlUrl=\"{}\"", escape_xml(website_url)));
        }

        // Add extended attributes
        if let Some(attrs_json) = &sub.opml_attributes {
            if let Ok(attrs) = serde_json::from_str::<HashMap<String, serde_json::Value>>(attrs_json) {
                for (key, value) in attrs {
                    if let Some(str_val) = value.as_str() {
                        xml.push_str(&format!(" {}=\"{}\"", escape_xml(&key), escape_xml(str_val)));
                    }
                }
            }
        }

        xml.push_str("/>");
    }

    xml.push_str("</body>");
    xml.push_str("</opml>");

    Ok(xml)
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
