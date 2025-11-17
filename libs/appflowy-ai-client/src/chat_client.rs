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
    let messages = self.build_messages(params);

    let body = json!({
      "model": self.deepseek_model,
      "messages": messages,
      "stream": true,
    });

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
    let messages = self.build_messages(params);

    let body = json!({
      "model": model_name,
      "messages": messages,
      "stream": true,
    });

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

  /// 调用豆包 API
  async fn stream_doubao(
    &self,
    params: &ChatRequestParams,
  ) -> Result<Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>> {
    if self.doubao_api_key.is_empty() {
      return Err(anyhow!("Doubao API key not configured"));
    }

    let url = format!("{}/chat/completions", self.doubao_api_base);
    let messages = self.build_messages(params);

    let body = json!({
      "model": self.doubao_model,
      "messages": messages,
      "stream": true,
    });

    debug!("Doubao request URL: {}", url);

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

  /// 构建消息列表 (支持历史对话和多模态)
  fn build_messages(&self, params: &ChatRequestParams) -> Vec<serde_json::Value> {
    let mut messages = Vec::new();

    // 添加历史消息
    for msg in &params.history {
      messages.push(json!({
        "role": msg.role,
        "content": msg.content,
      }));
    }

    // 添加当前消息
    if params.has_images && params.images.is_some() {
      // 多模态消息
      let mut content = vec![json!({
        "type": "text",
        "text": params.message,
      })];

      if let Some(images) = &params.images {
        for image_base64 in images {
          content.push(json!({
            "type": "image_url",
            "image_url": {
              "url": format!("data:image/jpeg;base64,{}", image_base64)
            }
          }));
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

