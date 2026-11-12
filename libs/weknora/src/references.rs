//! Reference IDs are resolved from the authenticated catalog, never trusted as tenant IDs.
use crate::{agent::Scope, managed, optional_array, text, Client};
use anyhow::{anyhow, ensure, Result};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
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
  pub async fn reference_catalog(
    &self,
    account: &Value,
    wid: Uuid,
    scope: &Scope,
  ) -> Result<Catalog> {
    let mut catalog = Catalog {
      items: vec![],
      sources: scope.documents.clone(),
    };
    let wid = wid.to_string();
    let current = account["workspaces"][&wid]["id"].as_str();
    let managed_bases: HashSet<_> = account["workspaces"]
      .as_object()
      .into_iter()
      .flat_map(|m| m.values())
      .filter_map(|w| w["id"].as_str())
      .collect();
    for (id, note) in &scope.notes {
      // Explicit notes use the live Cloud document instead of a potentially stale search copy.
      catalog.items.push(json!({"kind":"note","id":id,"name":note["title"],"subtitle":note["path"],"knowledge_ids":[],"url":note["url"]}));
    }
    if let Some(kb) = current {
      catalog.items.push(json!({"kind":"base","id":kb,"name":scope.workspace_name,"subtitle":"当前工作区笔记","visibility":"private","knowledge_ids":scope.documents.keys().collect::<Vec<_>>()}));
    }
    let owned = self
      .agent_request(&scope.token, "knowledge-bases", Method::GET, None)
      .await?;
    let shared = self
      .agent_request(&scope.token, "shared-knowledge-bases", Method::GET, None)
      .await?;
    let mut seen = BTreeSet::new();
    for (base, visibility) in optional_array(&owned["data"])?
      .into_iter()
      .map(|b| (b, "private"))
      .chain(
        optional_array(&shared["data"])?
          .into_iter()
          .map(|b| (&b["knowledge_base"], "shared")),
      )
    {
      let kb = text(base, "id")?;
      Uuid::parse_str(kb)?;
      if (managed_bases.contains(kb) && Some(kb) != current) || !seen.insert(kb.to_owned()) {
        continue;
      }
      let name = text(base, "name")?;
      let mut ids = vec![];
      let mut total_read = 0;
      for page in 1.. {
        let response = self
          .agent_request(
            &scope.token,
            &format!("knowledge-bases/{kb}/knowledge?page={page}&page_size=100"),
            Method::GET,
            None,
          )
          .await?;
        let entries = optional_array(&response["data"])?;
        let total = response["total"]
          .as_u64()
          .ok_or_else(|| anyhow!("missing knowledge total"))? as usize;
        let count = entries.len();
        for entry in entries {
          // Synced note copies must always pass Cloud's page visibility checks.
          if managed(entry["channel"].as_str().unwrap_or("")) {
            continue;
          }
          let id = text(entry, "id")?;
          Uuid::parse_str(id)?;
          ensure!(
            !catalog.sources.contains_key(id),
            "duplicate knowledge source"
          );
          let title = entry["file_name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| entry["title"].as_str())
            .unwrap_or("未命名文件");
          catalog.sources.insert(
            id.to_owned(),
            json!({"title":title,"url":format!("ponynotes://knowledge-file?id={id}"),"base_id":kb}),
          );
          ids.push(id.to_owned());
          if entry["type"] == "file" || entry["file_name"].as_str().is_some_and(|s| !s.is_empty()) {
            catalog.items.push(json!({"kind":"file","id":id,"name":title,"subtitle":name,"base_id":kb,"knowledge_ids":[id]}));
          }
        }
        total_read += count;
        if total_read == total {
          break;
        }
        ensure!(
          count > 0 && total_read < total,
          "incomplete knowledge pagination"
        );
      }
      if Some(kb) == current {
        let item = catalog
          .items
          .iter_mut()
          .find(|item| item["kind"] == "base" && item["id"] == kb)
          .ok_or_else(|| anyhow!("missing workspace base"))?;
        item["knowledge_ids"]
          .as_array_mut()
          .ok_or_else(|| anyhow!("invalid workspace base"))?
          .extend(ids.into_iter().map(Value::String));
      } else {
        catalog.items.push(json!({"kind":"base","id":kb,"name":name,"subtitle":if visibility == "shared" {"共享知识库"}else{"私有知识库"},"visibility":visibility,"knowledge_ids":ids}));
      }
    }
    Ok(catalog)
  }

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
