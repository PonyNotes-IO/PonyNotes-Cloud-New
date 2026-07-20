use std::collections::HashMap;

use anyhow::Error;
use async_trait::async_trait;
use collab::core::origin::CollabOrigin;

use collab::preclude::Collab;
use collab_database::database::timestamp;
use collab_document::blocks::DocumentData;
use collab_document::document::Document;
use collab_entity::CollabType;
use collab_folder::ViewLayout;
use serde_json::Value;

use crate::document::parser::JsonToDocumentParser;
use crate::document::util::create_document_from_json;
use crate::hierarchy_builder::WorkspaceViewBuilder;
use crate::{gen_view_id, TemplateData, TemplateObjectId, WorkspaceTemplate};

// Template Folder Structure:
// |-- 默认的工作空间 (space)
//     |-- 写给第一批测试用户的一封信 (document)
//
// 【默认工作区精简 2026-07-20】原为 General + Shared 两个空间、共 7 个内置视图
// （Getting started 及其 3 个子指南、To-dos 看板、空的 Shared 空间）。
// 现按产品要求精简为「一个空间 + 一封欢迎信」，用户可自行删除或保留。
//
// 注意：本文件是**服务端**模板，新建工作区(biz/workspace/ops.rs)与注册
// (biz/user/user_verify.rs)都走这里；客户端 PonyNotes-New 下有一份同名副本，
// 但对云端账号不生效，两处需同步修改。
// Note: Update the folder structure above if you changed the code below
pub struct GettingStartedTemplate;

impl GettingStartedTemplate {
  /// Create a document template data from the given JSON string
  ///
  /// Create a series of database templates from the given JSON String
  ///
  /// Notes: The output contains DatabaseCollab, DatabaseRowCollab
  async fn create_document_data(
    &self,
    general_view_uuid: String,
    welcome_letter_view_uuid: String,
  ) -> anyhow::Result<(TemplateData, TemplateData)> {
    let default_space_json = include_str!("../../assets/default_space.json");
    let general_data =
      create_document_from_json(general_view_uuid.clone(), default_space_json).await?;

    // 欢迎信：内容源自《写给第一批测试用户的一封信.md》。
    let welcome_letter_json = include_str!("../../assets/welcome_letter.json");
    let welcome_letter_data =
      create_document_from_json(welcome_letter_view_uuid.clone(), welcome_letter_json).await?;

    Ok((general_data, welcome_letter_data))
  }

}

#[async_trait]
impl WorkspaceTemplate for GettingStartedTemplate {
  fn layout(&self) -> ViewLayout {
    ViewLayout::Document
  }

  async fn create(&self, _object_id: String) -> anyhow::Result<Vec<TemplateData>> {
    unreachable!("This function is not supposed to be called.")
  }

  async fn create_workspace_view(
    &self,
    _uid: i64,
    workspace_view_builder: &mut WorkspaceViewBuilder,
  ) -> anyhow::Result<Vec<TemplateData>> {
    let general_view_uuid = gen_view_id().to_string();
    let welcome_letter_view_uuid = gen_view_id().to_string();

    let (general_data, welcome_letter_data) = self
      .create_document_data(
        general_view_uuid.clone(),
        welcome_letter_view_uuid.clone(),
      )
      .await?;

    // 唯一的默认空间，内含一封欢迎信。二者用户均可自行删除或保留。
    workspace_view_builder
      .with_view_builder(|view_builder| async {
        let created_at = timestamp();
        let mut view_builder = view_builder
          .with_view_id(general_view_uuid.clone())
          .with_name("默认的工作空间")
          .with_extra(&format!(
              "{{\"is_space\":true,\"space_icon\":\"interface_essential/home-3\",\"space_icon_color\":\"0xFFA34AFD\",\"space_permission\":0,\"space_created_at\":{}}}",
              created_at
          ));

        view_builder = view_builder
          .with_child_view_builder(|child_view_builder| async {
            let child_view_builder = child_view_builder
              .with_view_id(welcome_letter_view_uuid.clone())
              .with_name("写给第一批测试用户的一封信")
              .with_icon("💌");
            child_view_builder.build()
          })
          .await;

        view_builder.build()
      })
      .await;

    Ok(vec![general_data, welcome_letter_data])
  }
}

pub enum DocumentTemplateContent {
  Json(String),
  Data(DocumentData),
}

/// Create a document with the given content
pub struct DocumentTemplate(DocumentData);

impl DocumentTemplate {
  pub fn from_json(json: &str) -> Result<Self, Error> {
    let data = JsonToDocumentParser::json_str_to_document(json)?;
    Ok(Self(data))
  }

  pub fn from_data(data: DocumentData) -> Self {
    Self(data)
  }
}

#[async_trait]
impl WorkspaceTemplate for DocumentTemplate {
  fn layout(&self) -> ViewLayout {
    ViewLayout::Document
  }

  async fn create(&self, object_id: String) -> anyhow::Result<Vec<TemplateData>> {
    let options = collab::core::collab::CollabOptions::new(
      object_id.clone(),
      collab::core::collab::default_client_id(),
    );
    let collab = Collab::new_with_options(CollabOrigin::Empty, options)?;
    let document = Document::create_with_data(collab, self.0.clone())?;
    let data = document.encode_collab()?;
    Ok(vec![TemplateData {
      template_id: TemplateObjectId::Document(object_id),
      collab_type: CollabType::Document,
      encoded_collab: data,
    }])
  }

  async fn create_workspace_view(
    &self,
    _uid: i64,
    workspace_view_builder: &mut WorkspaceViewBuilder,
  ) -> anyhow::Result<Vec<TemplateData>> {
    let view_id = gen_view_id().to_string();

    workspace_view_builder
      .with_view_builder(|view_builder| async {
        view_builder
          .with_name("Getting started")
          .with_icon("⭐️")
          .with_view_id(view_id.clone())
          .build()
      })
      .await;

    self.create(view_id).await
  }
}

pub fn getting_started_document_data() -> Result<DocumentData, Error> {
  let json_str = include_str!("../../assets/getting_started.json");
  JsonToDocumentParser::json_str_to_document(json_str)
}

pub fn desktop_guide_document_data() -> Result<DocumentData, Error> {
  let json_str = include_str!("../../assets/desktop_guide.json");
  JsonToDocumentParser::json_str_to_document(json_str)
}

pub fn mobile_guide_document_data() -> Result<DocumentData, Error> {
  let json_str = include_str!("../../assets/mobile_guide.json");
  JsonToDocumentParser::json_str_to_document(json_str)
}

pub fn get_initial_document_data() -> Result<DocumentData, Error> {
  let json_str = include_str!("../../assets/initial_document.json");
  JsonToDocumentParser::json_str_to_document(json_str)
}

/// Replace the placeholders in the JSON value with the given replacements.
///
/// The placeholders are in the format of "<key>", for example "<name>".
/// The value of the placeholder will be replaced with the value of the key in the replacements map.
pub fn replace_json_placeholders(value: &mut Value, replacements: &HashMap<String, String>) {
  match value {
    Value::String(s) => {
      if s.starts_with('<') && s.ends_with('>') {
        let key = s.trim_start_matches('<').trim_end_matches('>');
        if let Some(replacement) = replacements.get(key) {
          *s = replacement.to_string();
        }
      }
    },
    Value::Array(arr) => {
      for item in arr {
        replace_json_placeholders(item, replacements);
      }
    },
    Value::Object(obj) => {
      for (_, v) in obj {
        replace_json_placeholders(v, replacements);
      }
    },
    _ => {},
  }
}
