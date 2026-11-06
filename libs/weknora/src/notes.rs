use crate::{array, text, Client};
use anyhow::{anyhow, ensure, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};

pub(crate) type Notes = BTreeMap<String, Value>;

fn display(value: &Value) -> String {
  value
    .as_str()
    .map(str::to_owned)
    .unwrap_or_else(|| value.to_string())
}

pub(crate) fn document_markdown(document: &Value) -> Result<String> {
  let blocks = document["blocks"]
    .as_object()
    .ok_or_else(|| anyhow!("missing document blocks"))?;
  let meta = &document["meta"];
  let mut pending = vec![(text(document, "page_id")?.to_owned(), 0usize)];
  let mut visited = HashSet::new();
  let mut output = Vec::new();
  while let Some((id, depth)) = pending.pop() {
    ensure!(visited.insert(id.clone()), "cyclic document");
    let block = blocks
      .get(&id)
      .ok_or_else(|| anyhow!("missing document block"))?;
    let data = match &block["data"] {
      Value::String(s) => serde_json::from_str(s)?,
      Value::Null => json!({}),
      other => other.clone(),
    };
    let external = block["external_id"].as_str().filter(|s| !s.is_empty());
    let mut body = if let Some(external) = external {
      meta["text_map"][external]
        .as_str()
        .ok_or_else(|| anyhow!("missing document text"))?
        .to_owned()
    } else {
      String::new()
    };
    let kind = text(block, "ty")?;
    match kind {
      "heading" => {
        body = format!(
          "{} {body}",
          "#".repeat(data["level"].as_u64().unwrap_or(1).clamp(1, 6) as usize)
        )
      },
      "bulleted_list" | "numbered_list" | "todo_list" => {
        let prefix = match kind {
          "numbered_list" => "1. ",
          "todo_list" if data["checked"] == true => "- [x] ",
          "todo_list" => "- [ ] ",
          _ => "- ",
        };
        body = format!("{}{prefix}{body}", "  ".repeat(depth.saturating_sub(1)));
      },
      "quote" | "callout" => body = format!("> {}", body.replace('\n', "\n> ")),
      "code" => {
        let fence = "`".repeat(
          body
            .split(|c| c != '`')
            .map(str::len)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
            .max(3),
        );
        body = format!(
          "{fence}{}\n{body}\n{fence}",
          data["language"].as_str().unwrap_or("")
        );
      },
      "divider" => body = "---".into(),
      // ponytail: preserve binary references; download/OCR belongs to a separate ingestion task.
      "image" | "file" | "video" | "audio" | "link_preview" | "bookmark" => {
        body += &format!("\n{data}")
      },
      _ if external.is_none()
        && !matches!(kind, "page" | "paragraph" | "toggle_list")
        && data != json!({}) =>
      {
        body += &format!("\n{data}")
      },
      _ => {},
    }
    if !body.trim().is_empty() {
      output.push(body);
    }
    if let Some(children) = block["children"]
      .as_str()
      .and_then(|k| meta["children_map"].get(k))
    {
      for child in array(children)?.iter().rev() {
        pending.push((
          child
            .as_str()
            .ok_or_else(|| anyhow!("invalid child ID"))?
            .to_owned(),
          depth + 1,
        ));
      }
    }
  }
  Ok(output.join("\n\n"))
}

pub(crate) fn quick_markdown(value: &Value) -> Result<String> {
  fn walk(value: &Value, depth: usize) -> Result<String> {
    ensure!(depth < 128, "quick-note nesting limit exceeded");
    if let Some(list) = value.as_array() {
      return list
        .iter()
        .map(|v| walk(v, depth + 1))
        .collect::<Result<Vec<_>>>()
        .map(|s| s.concat());
    }
    if !value.is_object() {
      return Ok(String::new());
    }
    if let Some(insert) = value.get("insert") {
      return Ok(display(insert));
    }
    let mut result = String::new();
    for key in ["delta", "attributes", "data", "children", "document", "ops"] {
      if let Some(child) = value.get(key) {
        result += &walk(child, depth + 1)?;
      }
    }
    if value["type"].as_str().is_some_and(|s| !s.is_empty()) {
      result.push('\n');
    }
    Ok(result)
  }
  walk(value, 0)
}

pub(crate) fn note_parts(
  wid: &str,
  kind: &str,
  oid: &str,
  title: &str,
  body: &str,
  location: &str,
  view: Option<&str>,
) -> Result<Notes> {
  let title: String = title
    .trim()
    .chars()
    .filter(|c| !c.is_control() && *c != '<' && *c != '>')
    .take(180)
    .collect();
  let title = if title.is_empty() {
    "未命名笔记"
  } else {
    &title
  };
  let mut header = format!("来源：小马笔记\n\n目录：{location}\n\n");
  if kind != "quick" {
    header += &format!(
      "[在小马笔记中打开](ponynotes://open?viewId={}&workspaceId={wid})\n\n",
      view.unwrap_or(oid)
    );
  }
  let size = 190000usize
    .checked_sub(header.chars().count())
    .filter(|n| *n > 0)
    .ok_or_else(|| anyhow!("note path too long"))?;
  let chars: Vec<char> = if body.is_empty() {
    "（空白笔记）".chars().collect()
  } else {
    body.chars().collect()
  };
  let total = chars.len().div_ceil(size);
  let mut notes = Notes::new();
  for (index, chunk) in chars.chunks(size).enumerate() {
    let identity = format!("{wid}/{kind}/{oid}/{index}");
    let channel = format!("pn:{:x}", Sha256::digest(identity.as_bytes()))[..43].to_owned();
    let label = if total > 1 {
      format!("{title}（{}/{total}）", index + 1)
    } else {
      title.to_owned()
    };
    notes.insert(
      channel,
      json!({"title":label,"content":header.clone()+&chunk.iter().collect::<String>()}),
    );
  }
  Ok(notes)
}

impl Client {
  pub(crate) async fn scan(&self, token: &str, wid: &str) -> Result<Notes> {
    let views = self.views(token, wid).await?;
    let mut notes = Notes::new();
    let mut database_views = HashSet::new();
    for (view, path) in &views {
      let oid = text(view, "view_id")?;
      if oid == wid || view["is_space"] == true {
        continue;
      }
      match view["layout"].as_i64() {
        Some(0) => {
          let doc = self
            .cloud(
              token,
              &format!("workspace/v1/{wid}/collab/{oid}/json?collab_type=0"),
            )
            .await?;
          notes.extend(note_parts(
            wid,
            "document",
            oid,
            text(view, "name")?,
            &document_markdown(&doc["collab"]["document"])?,
            path,
            None,
          )?);
        },
        Some(1..=3) => {
          database_views.insert(oid.to_owned());
        },
        Some(4) => {}, // AI conversations are outside the cloud-note scope.
        _ => anyhow::bail!("unsupported view layout"),
      }
    }
    let databases = self
      .cloud(token, &format!("workspace/{wid}/database"))
      .await?;
    let mut covered = HashSet::new();
    for db in array(&databases)? {
      let mut allowed = array(&db["views"])?
        .iter()
        .filter_map(|v| v["view_id"].as_str())
        .filter(|id| database_views.contains(*id))
        .collect::<Vec<_>>();
      allowed.sort();
      let Some(anchor) = allowed.first().copied() else {
        continue;
      };
      covered.extend(allowed.iter().map(|s| s.to_string()));
      let (view, path) = views
        .iter()
        .find(|(v, _)| v["view_id"] == anchor)
        .ok_or_else(|| anyhow!("missing database view"))?;
      let dbid = text(db, "id")?;
      let base = format!("workspace/{wid}/database/{dbid}");
      let fields = self.cloud(token, &format!("{base}/fields")).await?;
      let mut fields = array(&fields)?.iter().collect::<Vec<_>>();
      fields.sort_by_key(|f| {
        (
          f["name"].as_str().unwrap_or(""),
          f["id"].as_str().unwrap_or(""),
        )
      });
      let primary = fields
        .iter()
        .find(|f| f["is_primary"] == true)
        .and_then(|f| f["name"].as_str());
      let schema = fields
        .iter()
        .map(|f| {
          Ok(format!(
            "- {} ({})",
            text(f, "name")?,
            text(f, "field_type")?
          ))
        })
        .collect::<Result<Vec<_>>>()?
        .join("\n");
      notes.extend(note_parts(
        wid,
        "database",
        dbid,
        text(view, "name")?,
        &schema,
        path,
        Some(anchor),
      )?);
      let rows = self.cloud(token, &format!("{base}/row")).await?;
      let ids = array(&rows)?
        .iter()
        .map(|r| text(r, "id").map(str::to_owned))
        .collect::<Result<Vec<_>>>()?;
      ensure!(
        ids.iter().collect::<HashSet<_>>().len() == ids.len(),
        "duplicate database rows"
      );
      for batch in ids.chunks(40) {
        let rows = self
          .cloud(
            token,
            &format!("{base}/row/detail?ids={}&with_doc=true", batch.join(",")),
          )
          .await?;
        let rows = array(&rows)?;
        let returned = rows
          .iter()
          .map(|r| text(r, "id").map(str::to_owned))
          .collect::<Result<Vec<_>>>()?;
        ensure!(
          returned.len() == batch.len()
            && returned.iter().collect::<HashSet<_>>() == batch.iter().collect::<HashSet<_>>(),
          "incomplete database rows"
        );
        for row in rows {
          ensure!(
            row["has_doc"] != true || row["doc"].is_string(),
            "missing row document"
          );
          let cells = row["cells"]
            .as_object()
            .ok_or_else(|| anyhow!("missing row cells"))?;
          let mut body = cells
            .iter()
            .map(|(k, v)| format!("{k}：{}", display(v)))
            .collect::<Vec<_>>()
            .join("\n\n");
          body += &format!("\n\n{}", row["doc"].as_str().unwrap_or(""));
          let label = primary
            .and_then(|key| cells.get(key))
            .map(display)
            .unwrap_or_else(|| "数据库记录".into());
          notes.extend(note_parts(
            wid,
            "row",
            text(row, "id")?,
            &label,
            &body,
            path,
            Some(anchor),
          )?);
        }
      }
    }
    ensure!(
      covered == database_views,
      "database view missing from listing"
    );
    let mut offset = 0;
    let mut seen = HashSet::new();
    loop {
      let page = self
        .cloud(
          token,
          &format!("workspace/{wid}/quick-note?offset={offset}&limit=100"),
        )
        .await?;
      let batch = array(&page["quick_notes"])?;
      for note in batch {
        let id = text(note, "id")?;
        ensure!(seen.insert(id.to_owned()), "unstable quick-note pagination");
        let body = quick_markdown(&note["data"])?;
        ensure!(
          !body.trim().is_empty() || note["data"] == json!([]) || note["data"].is_null(),
          "unsupported quick-note format"
        );
        let date = text(note, "created_at")?
          .chars()
          .take(10)
          .collect::<String>();
        notes.extend(note_parts(
          wid,
          "quick",
          id,
          &format!("随手记 {date}"),
          &body,
          "随手记",
          None,
        )?);
      }
      if page["has_more"] == false {
        break;
      }
      ensure!(
        page["has_more"] == true && !batch.is_empty(),
        "incomplete quick-note pagination"
      );
      offset += batch.len();
    }
    Ok(notes)
  }
}
