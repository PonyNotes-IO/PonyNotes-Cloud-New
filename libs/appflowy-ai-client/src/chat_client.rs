use anyhow::{anyhow, Result};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde_json::json;
use std::pin::Pin;
use tracing::{debug, error, info};

use crate::dto::{AIModel, ChatRequestParams};

/// 统一的第三方 AI 聊天客户端
/// 支持 DeepSeek、通义千问、豆包等多个 AI 提供商
pub struct ChatClient {
  http_client: Client,
  // DeepSeek 配置
  deepseek_api_key: String,
  deepseek_api_base: String,
  deepseek_model: String,
  // 通义千问配置
  qwen_api_key: String,
  qwen_api_base: String,
  qwen_turbo_model: String,
  qwen_max_model: String,
  // 豆包配置
  doubao_api_key: String,
  doubao_api_base: String,
  doubao_model: String,
}

impl ChatClient {
  pub fn from_env() -> Result<Self> {
    // 读取环境变量
    let deepseek_api_key = std::env::var("AI_CHAT_DEEPSEEK_API_KEY")
      .unwrap_or_else(|_| String::new());
    let qwen_api_key = std::env::var("AI_CHAT_QWEN_API_KEY")
      .unwrap_or_else(|_| String::new());
    let doubao_api_key = std::env::var("AI_CHAT_DOUBAO_API_KEY")
      .unwrap_or_else(|_| String::new());
    
    // 详细日志：环境变量加载情况
    info!("ChatClient initialization:");
    info!("  - DeepSeek API Key: {} bytes", if deepseek_api_key.is_empty() { 0 } else { deepseek_api_key.len() });
    info!("  - Qwen API Key: {} bytes", if qwen_api_key.is_empty() { 0 } else { qwen_api_key.len() });
    info!("  - Doubao API Key: {} bytes", if doubao_api_key.is_empty() { 0 } else { doubao_api_key.len() });
    
    if deepseek_api_key.is_empty() && qwen_api_key.is_empty() && doubao_api_key.is_empty() {
      error!("WARNING: All AI provider API keys are empty! Chat functionality will not work.");
    }
    
    Ok(Self {
      http_client: Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?,
      deepseek_api_key,
      deepseek_api_base: std::env::var("AI_CHAT_DEEPSEEK_API_BASE")
        .unwrap_or_else(|_| "https://ark.cn-beijing.volces.com/api/v3".to_string()),
      deepseek_model: std::env::var("AI_CHAT_DEEPSEEK_MODEL")
        .unwrap_or_else(|_| "deepseek-v3-250324".to_string()),
      qwen_api_key,
      qwen_api_base: std::env::var("AI_CHAT_QWEN_API_BASE").unwrap_or_else(|_| {
        "https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()
      }),
      qwen_turbo_model: std::env::var("AI_CHAT_QWEN_TURBO_MODEL")
        .unwrap_or_else(|_| "qwen-turbo".to_string()),
      qwen_max_model: std::env::var("AI_CHAT_QWEN_MAX_MODEL")
        .unwrap_or_else(|_| "qwen-max".to_string()),
      doubao_api_key,
      doubao_api_base: std::env::var("AI_CHAT_DOUBAO_API_BASE")
        .unwrap_or_else(|_| "https://ark.cn-beijing.volces.com/api/v3".to_string()),
      doubao_model: std::env::var("AI_CHAT_DOUBAO_MODEL")
        .unwrap_or_else(|_| "ep-m-20250814175607-b77g6".to_string()),
    })
  }

  /// 统一的流式聊天接口
  pub async fn stream_chat(
    &self,
    params: &ChatRequestParams,
    model: AIModel,
  ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>> {
    info!(
      "Starting chat stream with model: {:?}, message length: {}",
      model,
      params.message.len()
    );

    match model {
      AIModel::DeepSeek => self.stream_deepseek(params).await,
      AIModel::QwenTurbo => self.stream_qwen(params, &self.qwen_turbo_model).await,
      AIModel::QwenMax => self.stream_qwen(params, &self.qwen_max_model).await,
      AIModel::Doubao => self.stream_doubao(params).await,
    }
  }

  /// 调用 DeepSeek API
  async fn stream_deepseek(
    &self,
    params: &ChatRequestParams,
  ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>> {
    if self.deepseek_api_key.is_empty() {
      return Err(anyhow!("DeepSeek API key not configured"));
    }

    let url = format!("{}/chat/completions", self.deepseek_api_base);
    let messages = self.build_messages_for_openai_compatible(params);

    let mut body = json!({
      "model": self.deepseek_model,
      "messages": messages,
      "stream": true,
    });
    
    // 如果启用深度思考，添加 enable_thinking 参数
    if params.enable_thinking {
      body["enable_thinking"] = json!(true);
    }
    
    // 如果启用全网搜索，添加 web_search 参数
    if params.enable_web_search {
      body["web_search"] = json!(true);
    }

    debug!("DeepSeek request URL: {}", url);
    debug!("DeepSeek request body: {}", serde_json::to_string_pretty(&body)?);

    let response = self
      .http_client
      .post(&url)
      .header("Authorization", format!("Bearer {}", self.deepseek_api_key))
      .header("Content-Type", "application/json")
      .json(&body)
      .send()
      .await?;

    let status = response.status();
    if !status.is_success() {
      let error_text = response.text().await?;
      error!("DeepSeek API error: {} - {}", status, error_text);
      return Err(anyhow!("DeepSeek API error: {} - {}", status, error_text));
    }

    info!("DeepSeek API response status: {}", status);

    Ok(Box::pin(
      response
        .bytes_stream()
        .map(|result| result.map_err(|e| anyhow!("Stream error: {}", e))),
    ))
  }

  /// 调用通义千问 API
  async fn stream_qwen(
    &self,
    params: &ChatRequestParams,
    model_name: &str,
  ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>> {
    if self.qwen_api_key.is_empty() {
      return Err(anyhow!("Qwen API key not configured"));
    }

    let url = format!("{}/chat/completions", self.qwen_api_base);
    let messages = self.build_messages_for_openai_compatible(params);

    let mut body = json!({
      "model": model_name,
      "messages": messages,
      "stream": true,
    });
    
    // 通义千问：enable_search 直接在顶层
    if params.enable_web_search {
      body["enable_search"] = json!(true);
    }
    
    // 深度思考可以通过提示词实现，或者作为参数传递
    if params.enable_thinking {
      body["enable_thinking"] = json!(true);
    }

    debug!("Qwen request URL: {}", url);

    let response = self
      .http_client
      .post(&url)
      .header("Authorization", format!("Bearer {}", self.qwen_api_key))
      .header("Content-Type", "application/json")
      .json(&body)
      .send()
      .await?;

    let status = response.status();
    if !status.is_success() {
      let error_text = response.text().await?;
      error!("Qwen API error: {} - {}", status, error_text);
      return Err(anyhow!("Qwen API error: {} - {}", status, error_text));
    }

    info!("Qwen API response status: {}", status);

    Ok(Box::pin(
      response
        .bytes_stream()
        .map(|result| result.map_err(|e| anyhow!("Stream error: {}", e))),
    ))
  }

  /// 调用豆包 API（支持多模态）
  async fn stream_doubao(
    &self,
    params: &ChatRequestParams,
  ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>> {
    if self.doubao_api_key.is_empty() {
      return Err(anyhow!("Doubao API key not configured"));
    }

    // 检查是否有图片，如果有图片则使用 responses 接口（多模态），否则使用 chat/completions 接口
    let has_images = params.has_images && params.images.is_some() && !params.images.as_ref().unwrap().is_empty();
    
    if has_images {
      // 使用多模态接口 /responses
      self.stream_doubao_multimodal(params).await
    } else {
      // 使用普通聊天接口 /chat/completions
      self.stream_doubao_chat(params).await
    }
  }

  /// 调用豆包普通聊天 API
  async fn stream_doubao_chat(
    &self,
    params: &ChatRequestParams,
  ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>> {
    let url = format!("{}/chat/completions", self.doubao_api_base);
    let messages = self.build_messages_for_openai_compatible(params);

    let mut body = json!({
      "model": self.doubao_model,
      "messages": messages,
      "stream": true,
    });
    
    // 如果启用深度思考，添加 enable_thinking 参数
    if params.enable_thinking {
      body["enable_thinking"] = json!(true);
    }
    
    // 如果启用全网搜索，添加 web_search 参数
    if params.enable_web_search {
      body["web_search"] = json!(true);
    }

    debug!("Doubao chat request URL: {}", url);
    debug!("Doubao chat request body: {}", serde_json::to_string_pretty(&body)?);

    let response = self
      .http_client
      .post(&url)
      .header("Authorization", format!("Bearer {}", self.doubao_api_key))
      .header("Content-Type", "application/json")
      .json(&body)
      .send()
      .await?;

    let status = response.status();
    if !status.is_success() {
      let error_text = response.text().await?;
      error!("Doubao API error: {} - {}", status, error_text);
      return Err(anyhow!("Doubao API error: {} - {}", status, error_text));
    }

    info!("Doubao API response status: {}", status);

    Ok(Box::pin(
      response
        .bytes_stream()
        .map(|result| result.map_err(|e| anyhow!("Stream error: {}", e))),
    ))
  }

  /// 调用豆包多模态 API（使用 /responses 接口）
  async fn stream_doubao_multimodal(
    &self,
    params: &ChatRequestParams,
  ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>> {
    let url = format!("{}/responses", self.doubao_api_base);
    let input = self.build_input_for_doubao_multimodal(params);

    let body = json!({
      "model": self.doubao_model,
      "input": input,
    });

    debug!("Doubao multimodal request URL: {}", url);
    debug!("Doubao multimodal request body: {}", serde_json::to_string_pretty(&body)?);

    let response = self
      .http_client
      .post(&url)
      .header("Authorization", format!("Bearer {}", self.doubao_api_key))
      .header("Content-Type", "application/json")
      .json(&body)
      .send()
      .await?;

    let status = response.status();
    if !status.is_success() {
      let error_text = response.text().await?;
      error!("Doubao multimodal API error: {} - {}", status, error_text);
      return Err(anyhow!("Doubao multimodal API error: {} - {}", status, error_text));
    }

    info!("Doubao multimodal API response status: {}", status);

    Ok(Box::pin(
      response
        .bytes_stream()
        .map(|result| result.map_err(|e| anyhow!("Stream error: {}", e))),
    ))
  }

  /// 构建消息列表 (OpenAI兼容格式，支持历史对话、多模态和文件)
  fn build_messages_for_openai_compatible(&self, params: &ChatRequestParams) -> Vec<serde_json::Value> {
    let mut messages = Vec::new();

    // 添加历史消息
    for msg in &params.history {
      messages.push(json!({
        "role": msg.role,
        "content": msg.content,
      }));
    }

    // 判断是否需要使用多模态格式 (图片或文件)
    let has_multimodal = (params.has_images && params.images.is_some()) 
                      || (params.has_files && params.files.is_some());

    if has_multimodal {
      // 多模态消息
      let mut content = vec![json!({
        "type": "text",
        "text": self.build_message_text_with_files(params),
      })];

      // 添加图片（OpenAI格式：使用data URL或http URL）
      if let Some(images) = &params.images {
        for image_data in images {
          // 判断是URL还是base64
          if image_data.starts_with("http://") || image_data.starts_with("https://") {
            // 如果已经是URL，直接使用
            content.push(json!({
              "type": "image_url",
              "image_url": {
                "url": image_data
              }
            }));
          } else {
            // 如果是base64，添加data URL前缀
            content.push(json!({
              "type": "image_url",
              "image_url": {
                "url": format!("data:image/jpeg;base64,{}", image_data)
              }
            }));
          }
        }
      }

      messages.push(json!({
        "role": "user",
        "content": content,
      }));
    } else {
      // 纯文本消息
      messages.push(json!({
        "role": "user",
        "content": params.message,
      }));
    }

    messages
  }

  /// 构建豆包多模态 API 的 input 字段
  fn build_input_for_doubao_multimodal(&self, params: &ChatRequestParams) -> Vec<serde_json::Value> {
    let mut input = Vec::new();

    // 添加历史消息（如果有）
    for msg in &params.history {
      // 豆包的历史消息格式与OpenAI类似
      input.push(json!({
        "role": msg.role,
        "content": [
          {
            "type": "input_text",
            "text": msg.content,
          }
        ],
      }));
    }

    // 构建当前用户消息
    let mut content = Vec::new();

    // 添加图片（豆包格式：type: "input_image", image_url: "URL"）
    if let Some(images) = &params.images {
      for image_url in images {
        // 豆包只接受URL，不接受base64
        if image_url.starts_with("http://") || image_url.starts_with("https://") {
          content.push(json!({
            "type": "input_image",
            "image_url": image_url,
          }));
        } else {
          error!("Doubao multimodal API requires image URLs, but got non-URL data");
          // 跳过非URL格式的图片
        }
      }
    }

    // 添加文本（豆包格式：type: "input_text", text: "内容"）
    let text_content = self.build_message_text_with_files(params);
    content.push(json!({
      "type": "input_text",
      "text": text_content,
    }));

    // 添加用户消息
    input.push(json!({
      "role": "user",
      "content": content,
    }));

    input
  }

  /// 构建包含文件内容的消息文本
  fn build_message_text_with_files(&self, params: &ChatRequestParams) -> String {
    let mut text = params.message.clone();
    
    // 添加文件内容到消息中
    if let Some(files) = &params.files {
      if !files.is_empty() {
        text.push_str("\n\n--- 附件文件 ---\n");
        for file in files {
          text.push_str(&format!("\n📄 文件名: {}\n", file.file_name));
          text.push_str(&format!("文件类型: {}\n", file.file_type));
          text.push_str(&format!("文件大小: {} 字节\n", file.file_size));
          
          match &file.file_data {
            crate::dto::FileData::Text(content) => {
              text.push_str("文件内容:\n```\n");
              text.push_str(content);
              text.push_str("\n```\n");
            },
            crate::dto::FileData::Url(url) => {
              text.push_str(&format!("文件URL: {}\n", url));
            },
            crate::dto::FileData::Base64(base64_content) => {
              // 对于base64，尝试判断是否是文本文件
              if Self::is_text_file_type(&file.file_type) {
                if let Ok(decoded) = base64::decode(base64_content) {
                  if let Ok(content) = String::from_utf8(decoded) {
                    text.push_str("文件内容:\n```\n");
                    // 限制文件内容长度，避免超出token限制
                    if content.len() > 50000 {
                      text.push_str(&content[..50000]);
                      text.push_str("\n... (内容过长，已截断) ...\n");
                    } else {
                      text.push_str(&content);
                    }
                    text.push_str("\n```\n");
                  }
                }
              } else {
                text.push_str("[二进制文件内容]\n");
              }
            },
          }
        }
      }
    }
    
    text
  }

  /// 判断是否是文本文件类型
  fn is_text_file_type(file_type: &str) -> bool {
    matches!(
      file_type.to_lowercase().as_str(),
      "txt" | "md" | "markdown" | "json" | "xml" | "html" | "css" | "js" 
      | "ts" | "jsx" | "tsx" | "py" | "rs" | "go" | "java" | "c" | "cpp" 
      | "h" | "hpp" | "sh" | "bash" | "yaml" | "yml" | "toml" | "ini" 
      | "log" | "csv" | "sql"
    )
  }

  /// 检查指定模型是否可用
  pub fn is_model_available(&self, model: AIModel) -> bool {
    match model {
      AIModel::DeepSeek => !self.deepseek_api_key.is_empty(),
      AIModel::QwenTurbo | AIModel::QwenMax => !self.qwen_api_key.is_empty(),
      AIModel::Doubao => !self.doubao_api_key.is_empty(),
    }
  }

  /// 获取可用的模型列表
  pub fn get_available_models(&self) -> Vec<AIModel> {
    let mut models = Vec::new();
    if !self.deepseek_api_key.is_empty() {
      models.push(AIModel::DeepSeek);
    }
    if !self.qwen_api_key.is_empty() {
      models.push(AIModel::QwenTurbo);
      models.push(AIModel::QwenMax);
    }
    if !self.doubao_api_key.is_empty() {
      models.push(AIModel::Doubao);
    }
    models
  }
}

