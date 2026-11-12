//! Adapt mainstream attachments to WeKnora's native parsers without changing original bytes.
use anyhow::{bail, ensure, Result};
use serde_json::Value;

pub const NATIVE_EXTENSIONS: &[&str] = &[
  "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "markdown", "csv", "json",
  "xml", "yaml", "yml", "log", "html", "mhtml", "epub", "xmind", "png", "jpg", "jpeg", "webp",
  "gif", "bmp", "tiff",
];
pub const CODE_EXTENSIONS: &[&str] = &[
  "dart", "rs", "js", "jsx", "ts", "tsx", "py", "java", "kt", "kts", "swift", "go", "c", "h",
  "cpp", "hpp", "cs", "php", "rb", "sql", "sh", "bash", "ps1", "css", "scss", "vue", "svelte",
  "toml", "ini", "cfg",
];

pub fn upload_name(name: &str) -> Result<String> {
  ensure!(
    !name.is_empty()
      && name.len() <= 255
      && !name.contains(['/', '\\', ':'])
      && !name.chars().any(char::is_control),
    "附件名称无效"
  );
  let ext = name
    .rsplit_once('.')
    .map(|(_, ext)| ext.to_ascii_lowercase())
    .unwrap_or_default();
  let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
  let upload = match ext.as_str() {
    "tif" => format!("{stem}.tiff"),
    "htm" => format!("{stem}.html"),
    "text" => format!("{stem}.txt"),
    ext if NATIVE_EXTENSIONS.contains(&ext) => format!("{stem}.{ext}"),
    // WeKnora rejects source extensions; .txt selects its complete text reader, not a binary parser.
    ext if CODE_EXTENSIONS.contains(&ext) => format!("{name}.txt"),
    _ => bail!("暂不支持此附件格式，请选择图片、文档、表格、演示文稿或文本代码文件"),
  };
  ensure!(upload.len() <= 255, "附件名称过长，请缩短后重试");
  Ok(upload)
}

pub fn mime_type(name: &str) -> &'static str {
  match name.rsplit('.').next().unwrap_or("") {
    "png" => "image/png",
    "jpg" | "jpeg" => "image/jpeg",
    "gif" => "image/gif",
    "webp" => "image/webp",
    "bmp" => "image/bmp",
    "tiff" => "image/tiff",
    "pdf" => "application/pdf",
    "doc" => "application/msword",
    "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "xls" => "application/vnd.ms-excel",
    "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "ppt" => "application/vnd.ms-powerpoint",
    "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "epub" => "application/epub+zip",
    "html" => "text/html",
    "mhtml" => "message/rfc822",
    "xmind" => "application/zip",
    _ => "text/plain; charset=utf-8",
  }
}

pub fn restore_names(message: &mut Value, names: &Value) {
  for file in message
    .get_mut("attachments")
    .and_then(Value::as_array_mut)
    .into_iter()
    .flatten()
  {
    if let Some(name) = file["id"]
      .as_str()
      .and_then(|id| names.get(id))
      .and_then(Value::as_str)
    {
      file["file_name"] = Value::String(name.to_owned());
    }
  }
}
