//! Reference IDs are resolved from the authenticated catalog, never trusted as tenant IDs.
use crate::{agent::Scope, Client};
use anyhow::{anyhow, ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Reference {
  pub kind: String,
  pub id: String,
}

impl Reference {
  pub fn valid(&self) -> bool {
    matches!(self.kind.as_str(), "base" | "file" | "note") && Uuid::parse_str(&self.id).is_ok()
  }
}

/// Restore the original input only for a context marker created by this Cloud session.
pub fn restore_reference_input(message: &mut Value, record: &Value) {
  if message["role"] != "user" {
    return;
  }
  let Some(content) = message["content"].as_str() else {
    return;
  };
  let original = record["input_contexts"]
    .as_array()
    .into_iter()
    .flatten()
    .find(|input| {
      let (Some(query), Some(id)) = (input["query"].as_str(), input["id"].as_str()) else {
        return false;
      };
      content.starts_with(&format!("{query}\n\n<reference_context id=\"{id}\">"))
    });
  if let Some(input) = original {
    message["content"] = input["query"].clone();
    message["mentioned_items"] = input["mentioned_items"].clone();
  }
}

pub struct Catalog {
  pub items: Vec<Value>,
  pub sources: BTreeMap<String, Value>,
}

impl Catalog {
  pub fn notes(scope: &Scope) -> Self {
    Self {
      items: scope.notes.iter().map(|(id, note)| {
        json!({"kind":"note","id":id,"name":note["title"],"subtitle":note["path"],
        "visibility":note["visibility"],"space_name":note["space_name"],"knowledge_ids":[],"url":note["url"]})
      }).collect(),
      sources: BTreeMap::new(),
    }
  }

  pub fn resolve(&self, references: &[Reference], strict: bool) -> Result<BTreeMap<String, Value>> {
    let mut sources = BTreeMap::new();
    for reference in references {
      let item = self
        .items
        .iter()
        .find(|item| item["kind"] == reference.kind && item["id"] == reference.id);
      if strict {
        ensure!(item.is_some(), "reference access denied");
        ensure!(
          reference.kind == "note"
            || item.is_some_and(|i| i["knowledge_ids"]
              .as_array()
              .is_some_and(|ids| !ids.is_empty())),
          "reference has no searchable content"
        );
      }
      if let Some(item) = item {
        if reference.kind == "note" {
          sources.insert(
            format!("note:{}", reference.id),
            json!({"title":item["name"],"url":item["url"]}),
          );
          continue;
        }
        for id in item["knowledge_ids"].as_array().into_iter().flatten() {
          let id = id
            .as_str()
            .ok_or_else(|| anyhow!("invalid reference source"))?;
          sources.insert(id.to_owned(), self.sources[id].clone());
        }
      }
    }
    Ok(sources)
  }

  pub fn mentions(&self, references: &[Reference]) -> Vec<Value> {
    self.items.iter().filter(|item| references.iter().any(|r| item["kind"] == r.kind && item["id"] == r.id))
      .map(|item| json!({"id":item["id"],"name":item["name"],"type":if item["kind"] == "base" {"kb"} else {"knowledge"}})).collect()
  }
}

impl Client {
  pub async fn reference_note_text(&self, token: &str, wid: Uuid, id: &str) -> Result<String> {
    Uuid::parse_str(id)?;
    let doc = self
      .cloud(
        token,
        &format!("workspace/v1/{wid}/collab/{id}/json?collab_type=0"),
      )
      .await?;
    crate::notes::document_markdown(&doc["collab"]["document"])
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn history_restores_only_cloud_generated_context_and_keeps_reference_labels() {
    let record = json!({"input_contexts":[{"id":"server-nonce","query":"总结笔记","mentioned_items":[{"name":"会议记录"}]}]});
    let mut message = json!({"role":"user","content":"总结笔记\n\n<reference_context id=\"server-nonce\">\nprivate material\n</reference_context>"});
    restore_reference_input(&mut message, &record);
    assert_eq!(message["content"], "总结笔记");
    assert_eq!(message["mentioned_items"][0]["name"], "会议记录");
    let mut ordinary = json!({"role":"user","content":"总结笔记\n\n<reference_context id=\"other\">literal user content"});
    let original = ordinary.clone();
    restore_reference_input(&mut ordinary, &record);
    assert_eq!(ordinary, original);
  }
  #[test]
  fn selection_rejects_unknown_and_revoked_ids_without_expanding_scope() {
    let catalog = Catalog {
      items: vec![json!({"kind":"base","id":"allowed","knowledge_ids":["file"]})],
      sources: BTreeMap::from([("file".into(), json!({"title":"allowed file"}))]),
    };
    let allowed = Reference {
      kind: "base".into(),
      id: "allowed".into(),
    };
    assert_eq!(catalog.resolve(&[allowed], true).unwrap().len(), 1);
    let denied = Reference {
      kind: "file".into(),
      id: "private".into(),
    };
    assert!(catalog
      .resolve(std::slice::from_ref(&denied), true)
      .is_err());
    assert!(catalog.resolve(&[denied], false).unwrap().is_empty());
    assert!(!Reference {
      kind: "file".into(),
      id: "../private".into()
    }
    .valid());
  }

  #[test]
  fn explicit_note_does_not_retrieve_its_stale_search_copy() {
    let catalog = Catalog {
      items: vec![
        json!({"kind":"note","id":"page","name":"Live note","url":"ponynotes://open?viewId=page","knowledge_ids":["old-copy"]}),
      ],
      sources: BTreeMap::from([("old-copy".into(), json!({"title":"Old content"}))]),
    };
    let resolved = catalog
      .resolve(
        &[Reference {
          kind: "note".into(),
          id: "page".into(),
        }],
        true,
      )
      .unwrap();
    assert_eq!(
      resolved.keys().cloned().collect::<Vec<_>>(),
      vec!["note:page"]
    );
  }
}
