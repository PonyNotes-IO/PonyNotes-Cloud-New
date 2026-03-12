use crate::api::util::ai_model_from_header;
use crate::biz::authentication::jwt::UserUuid;
use crate::biz::subscription::ops::{fetch_current_subscription, get_user_resource_limit_status, record_usage};
use database::subscription::get_user_total_usage_bytes;
use crate::biz::workspace::subscription_plan_limits::PlanLimits;
use crate::state::AppState;
use shared_entity::dto::subscription_dto::UsageRecordRequest;
use shared_entity::dto::subscription_dto::UsageType;

use actix_web::web::{Data, Json};
use actix_web::{web, HttpRequest, HttpResponse, Scope};
use app_error::AppError;
use appflowy_ai_client::dto::{
  CalculateSimilarityParams, LocalAIConfig, ModelList, SimilarityResponse, TranslateRowParams,
  TranslateRowResponse, STREAM_ANSWER_KEY,
};
use appflowy_ai_client::{AIModel, AIModelInfo, AvailableModelsResponse, ChatRequestParams};

use futures_util::{stream, Stream, StreamExt, TryStreamExt};
use bytes::Bytes;
use serde_json::Value;
use std::pin::Pin;
use appflowy_ai_client::error::AIError;

use database::ai_usage::{get_workspace_ai_usage_this_month, increment_ai_usage};
use serde::Deserialize;
use shared_entity::dto::ai_dto::{
  CompleteTextParams, SummarizeRowData, SummarizeRowParams, SummarizeRowResponse,
};
use shared_entity::dto::billing_dto::SubscriptionPlan;
use shared_entity::response::AppResponse;
use uuid::Uuid;

use tracing::{error, info, instrument, trace, warn};

pub fn ai_completion_scope() -> Scope {
  web::scope("/api/ai")
    // 公开接口（无需认证，无需workspace_id）
    .service(web::resource("/chat/models").route(web::get().to(list_chat_models_handler)))
    .service(web::resource("/chat/session").route(web::post().to(public_chat_session_handler)))
    .service(web::resource("/file/upload").route(web::post().to(upload_file_handler)))
    // workspace相关接口
    .service(web::scope("/{workspace_id}")
      .service(web::resource("/complete/stream").route(web::post().to(stream_complete_text_handler)))
      .service(web::resource("/v2/complete/stream").route(web::post().to(stream_complete_v2_handler)))
      .service(web::resource("/chat/stream").route(web::post().to(stream_chat_handler)))
      .service(web::resource("/summarize_row").route(web::post().to(summarize_row_handler)))
      .service(web::resource("/translate_row").route(web::post().to(translate_row_handler)))
      .service(web::resource("/local/config").route(web::get().to(local_ai_config_handler)))
      .service(
        web::resource("/calculate_similarity").route(web::post().to(calculate_similarity_handler)),
      )
      .service(web::resource("/model/list").route(web::get().to(model_list_handler)))
    )
}

async fn stream_complete_text_handler(
  _user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
  payload: Json<CompleteTextParams>,
  req: HttpRequest,
) -> actix_web::Result<HttpResponse> {
  let workspace_id = workspace_id.into_inner();
  let ai_model = ai_model_from_header(&req);
  let params = payload.into_inner();
  
  // Check AI usage limits
  if let Err(err) = check_ai_usage_limit(&state, &workspace_id).await {
    return Ok(
      HttpResponse::Ok()
        .content_type("text/event-stream")
        .streaming(stream::once(async move { Err(err) })),
    );
  }
  
  state.metrics.ai_metrics.record_total_completion_count(1);

  if let Some(prompt_id) = params
    .metadata
    .as_ref()
    .and_then(|metadata| metadata.prompt_id.as_ref())
  {
    state
      .metrics
      .ai_metrics
      .record_prompt_usage_count(prompt_id, 1);
  }

  match state
    .ai_client
    .stream_completion_text(params, ai_model)
    .await
  {
    Ok(stream) => {
      // Increment AI usage count after successful request
      if let Err(e) = increment_ai_usage(&state.pg_pool, &workspace_id).await {
        error!("Failed to increment AI usage: {:?}", e);
      }
      
      Ok(
        HttpResponse::Ok()
          .content_type("text/event-stream")
          .streaming(stream.map_err(AppError::from)),
      )
    },
    Err(err) => Ok(
      HttpResponse::Ok()
        .content_type("text/event-stream")
        .streaming(stream::once(async move {
          Err(AppError::AIServiceUnavailable(err.to_string()))
        })),
    ),
  }
}

/// 将OpenAI格式的SSE流转换为客户端期望的JSON格式的流转换器
/// OpenAI格式: data: {"choices":[{"delta":{"content":"text"}}]}\n\n
/// 客户端期望: {"1": "text"} 或 {"4": "comment"}
fn convert_openai_stream_to_json_stream(
  stream: impl Stream<Item = Result<Bytes, AIError>> + Send + 'static,
) -> impl Stream<Item = Result<Bytes, AppError>> + Send {
  use futures_util::stream::unfold;
  
  struct StreamState {
    stream: Pin<Box<dyn Stream<Item = Result<Bytes, AIError>> + Send>>,
    buffer: String,
  }
  
  let state = StreamState {
    stream: Box::pin(stream),
    buffer: String::new(),
  };
  
  unfold(state, |mut state| async move {
    use futures_util::StreamExt;
    
    // 从底层流获取下一个chunk
    match state.stream.next().await {
      Some(Ok(bytes)) => {
        // 将字节转换为字符串
        let text = match String::from_utf8(bytes.to_vec()) {
          Ok(s) => s,
          Err(_) => {
            // 如果不是UTF-8，直接返回原始字节（可能是二进制数据）
            return Some((Ok(bytes), state));
          }
        };
        
        // 将新数据追加到缓冲区
        state.buffer.push_str(&text);
        
        // 尝试从缓冲区提取完整的SSE事件
        let mut json_output = String::new();
        let mut processed_len = 0;
        
        // 按行处理
        let lines: Vec<&str> = state.buffer.lines().collect();
        let mut i = 0;
        while i < lines.len() {
          let line = lines[i].trim();
          
          // 跳过空行和event行
          if line.is_empty() || line.starts_with("event:") {
            i += 1;
            continue;
          }
          
          // 处理data行
          if let Some(data_str) = line.strip_prefix("data:") {
            let data_str = data_str.trim();
            
            // 检查是否是结束标记
            if data_str == "[DONE]" {
              i += 1;
              continue;
            }
            
            // 尝试解析JSON
            match serde_json::from_str::<Value>(data_str) {
              Ok(json) => {
                // 从OpenAI兼容格式转换为内部格式
                // OpenAI格式: {"choices":[{"delta":{"content":"text"}}]}
                // 内部格式: {"1": "text"} 或 {"4": "comment"}
                if let Value::Object(ref obj) = json {
                  if let Some(choices) = obj.get("choices").and_then(|c| c.as_array()) {
                    if let Some(first_choice) = choices.first() {
                      if let Some(delta) = first_choice.get("delta").and_then(|d| d.as_object()) {
                        // 优先使用 content，如果没有或为空则使用 reasoning_content（豆包模型的思考过程）
                        let content = delta.get("content")
                          .and_then(|c| c.as_str())
                          .filter(|s| !s.is_empty())
                          .or_else(|| {
                            delta.get("reasoning_content")
                              .and_then(|c| c.as_str())
                              .filter(|s| !s.is_empty())
                          });
                        
                        if let Some(content_text) = content {
                          // 转换为内部格式
                          let mut internal_obj = serde_json::Map::new();
                          internal_obj.insert(STREAM_ANSWER_KEY.to_string(), Value::String(content_text.to_string()));
                          
                          // 序列化为JSON字符串
                          if let Ok(json_str) = serde_json::to_string(&Value::Object(internal_obj)) {
                            json_output.push_str(&json_str);
                            json_output.push('\n');
                          }
                        }
                      }
                    }
                  }
                }
              },
              Err(e) => {
                // 如果解析失败，记录错误但继续处理
                trace!("Failed to parse SSE JSON: {} - {}", e, data_str);
              }
            }
          }
          
          // 计算已处理的数据长度（包括换行符）
          if i < lines.len() - 1 {
            // 不是最后一行，可以安全处理
            processed_len += lines[i].len() + 1; // +1 for newline
          } else {
            // 最后一行，可能不完整，保留在缓冲区
            break;
          }
          
          i += 1;
        }
        
        // 移除已处理的数据
        if processed_len > 0 {
          state.buffer.drain(0..processed_len);
        }
        
        // 如果有转换后的JSON输出，返回它；否则返回空字节（等待更多数据）
        if !json_output.is_empty() {
          Some((Ok(Bytes::from(json_output)), state))
        } else {
          // 返回空字节，表示需要等待更多数据
          Some((Ok(Bytes::new()), state))
        }
      },
      Some(Err(e)) => {
        Some((Err(AppError::AIServiceUnavailable(e.to_string())), state))
      },
      None => {
        // 流结束，处理剩余的缓冲区数据
        if !state.buffer.trim().is_empty() {
          // 尝试处理剩余的缓冲区数据
          // 这里可以添加最后的处理逻辑
        }
        None
      }
    }
  })
}

async fn stream_complete_v2_handler(
  _user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
  payload: Json<CompleteTextParams>,
  req: HttpRequest,
) -> actix_web::Result<HttpResponse> {
  let workspace_id = workspace_id.into_inner();
  let ai_model = ai_model_from_header(&req);
  let params = payload.into_inner();
  
  // Check AI usage limits
  if let Err(err) = check_ai_usage_limit(&state, &workspace_id).await {
    return Ok(
      HttpResponse::Ok()
        .content_type("text/event-stream")
        .streaming(stream::once(async move { Err(err) })),
    );
  }
  
  state.metrics.ai_metrics.record_total_completion_count(1);

  match state.ai_client.stream_completion_v2(params, ai_model).await {
    Ok(stream) => {
      // Increment AI usage count after successful request
      if let Err(e) = increment_ai_usage(&state.pg_pool, &workspace_id).await {
        error!("Failed to increment AI usage: {:?}", e);
      }
      
      // 转换OpenAI格式的流为客户端期望的JSON格式
      let converted_stream = convert_openai_stream_to_json_stream(stream)
        .map(|result| result.map_err(|e| actix_web::error::ErrorInternalServerError(e)));
      
      Ok(
        HttpResponse::Ok()
          .content_type("text/event-stream")
          .streaming(converted_stream),
      )
    },
    Err(err) => Ok(
      HttpResponse::Ok()
        .content_type("text/event-stream")
        .streaming(stream::once(async move {
          Err(AppError::AIServiceUnavailable(err.to_string()))
        })),
    ),
  }
}
#[instrument(level = "debug", skip(state, payload), err)]
async fn summarize_row_handler(
  state: Data<AppState>,
  payload: Json<SummarizeRowParams>,
  req: HttpRequest,
) -> actix_web::Result<Json<AppResponse<SummarizeRowResponse>>> {
  let params = payload.into_inner();
  match params.data {
    SummarizeRowData::Identity { .. } => {
      return Err(AppError::InvalidRequest("Identity data is not supported".to_string()).into());
    },
    SummarizeRowData::Content(content) => {
      if content.is_empty() {
        return Ok(
          AppResponse::Ok()
            .with_data(SummarizeRowResponse {
              text: "No content".to_string(),
            })
            .into(),
        );
      }

      state.metrics.ai_metrics.record_total_summary_row_count(1);
      let ai_model = ai_model_from_header(&req);
      let result = state.ai_client.summarize_row(&content, ai_model).await;
      let resp = match result {
        Ok(resp) => SummarizeRowResponse { text: resp.text },
        Err(err) => {
          error!("Failed to summarize row: {:?}", err);
          SummarizeRowResponse {
            text: "No content".to_string(),
          }
        },
      };

      Ok(AppResponse::Ok().with_data(resp).into())
    },
  }
}

#[instrument(level = "debug", skip(state, payload), err)]
async fn translate_row_handler(
  state: web::Data<AppState>,
  payload: web::Json<TranslateRowParams>,
  req: HttpRequest,
) -> actix_web::Result<Json<AppResponse<TranslateRowResponse>>> {
  let params = payload.into_inner();
  let ai_model = ai_model_from_header(&req);
  state.metrics.ai_metrics.record_total_translate_row_count(1);
  match state.ai_client.translate_row(params.data, ai_model).await {
    Ok(resp) => Ok(AppResponse::Ok().with_data(resp).into()),
    Err(err) => {
      error!("Failed to translate row: {:?}", err);
      Ok(
        AppResponse::Ok()
          .with_data(TranslateRowResponse::default())
          .into(),
      )
    },
  }
}

#[derive(Deserialize, Debug)]
struct ConfigQuery {
  platform: String,
  app_version: Option<String>,
}

#[instrument(level = "debug", skip_all, err)]
async fn local_ai_config_handler(
  state: web::Data<AppState>,
  query: web::Query<ConfigQuery>,
) -> actix_web::Result<Json<AppResponse<LocalAIConfig>>> {
  let query = query.into_inner();
  trace!("query ai configuration: {:?}", query);
  let platform = match query.platform.as_str() {
    "macos" => "macos",
    "linux" => "ubuntu",
    "ubuntu" => "ubuntu",
    "windows" => "windows",
    _ => {
      return Err(AppError::InvalidRequest("Invalid platform".to_string()).into());
    },
  };

  let config = state
    .ai_client
    .get_local_ai_config(platform, query.app_version)
    .await
    .map_err(|err| AppError::AIServiceUnavailable(err.to_string()))?;
  Ok(AppResponse::Ok().with_data(config).into())
}

/// Helper function to check if AI usage is within limits
async fn check_ai_usage_limit(state: &AppState, workspace_id: &Uuid) -> Result<(), AppError> {
  // Get workspace subscription plan
  let workspace = database::workspace::select_workspace(&state.pg_pool, workspace_id).await?;
  let plan = SubscriptionPlan::try_from(workspace.workspace_type)
    .map_err(|e| AppError::Internal(anyhow::anyhow!("Invalid workspace type: {}", e)))?;
  
  let limits = PlanLimits::from_plan(&plan);
  
  // If AI is unlimited, no need to check
  if limits.ai_unlimited {
    return Ok(());
  }
  
  // Get current AI usage this month
  let current_usage = get_workspace_ai_usage_this_month(&state.pg_pool, workspace_id).await?;
  
  // Check if usage limit is exceeded
  if !limits.can_use_ai(current_usage) {
    return Err(AppError::PlanLimitExceeded(format!(
      "AI response limit exceeded. Plan: {:?}, Current: {}, Limit: {}. Please upgrade your subscription.",
      plan, current_usage, limits.ai_responses_limit
    )));
  }
  
  Ok(())
}

#[instrument(level = "debug", skip_all, err)]
async fn calculate_similarity_handler(
  state: web::Data<AppState>,
  payload: web::Json<CalculateSimilarityParams>,
) -> actix_web::Result<Json<AppResponse<SimilarityResponse>>> {
  let params = payload.into_inner();

  let response = state
    .ai_client
    .calculate_similarity(params)
    .await
    .map_err(|err| AppError::AIServiceUnavailable(err.to_string()))?;
  Ok(AppResponse::Ok().with_data(response).into())
}

#[instrument(level = "debug", skip_all, err)]
async fn model_list_handler(
  state: web::Data<AppState>,
) -> actix_web::Result<Json<AppResponse<ModelList>>> {
  let model_list = state
    .ai_client
    .get_model_list()
    .await
    .map_err(|err| AppError::AIServiceUnavailable(err.to_string()))?;
  Ok(AppResponse::Ok().with_data(model_list).into())
}

/// 流式AI聊天接口 (使用第三方AI提供商)
#[instrument(level = "debug", skip(state, payload), err)]
async fn stream_chat_handler(
  user_uuid: UserUuid,
  workspace_id: web::Path<Uuid>,
  state: Data<AppState>,
  payload: Json<ChatRequestParams>,
) -> actix_web::Result<HttpResponse> {
  let workspace_id = workspace_id.into_inner();
  let params = payload.into_inner();
  
  trace!(
    "Chat request from user {} for workspace {}, message length: {}", 
    *user_uuid,
    workspace_id,
    params.message.len()
  );
  
  // 1. 检查 AI 使用限额
  if let Err(err) = check_ai_usage_limit(&state, &workspace_id).await {
    error!("AI usage limit exceeded for workspace {}: {:?}", workspace_id, err);
    return Ok(
      HttpResponse::PaymentRequired()
        .json(serde_json::json!({
          "code": "AI_LIMIT_EXCEEDED",
          "message": err.to_string(),
        }))
    );
  }
  
  // 2. 确定使用的模型
  let model = if let Some(model_id) = &params.preferred_model {
    AIModel::from_str(model_id).unwrap_or(AIModel::DeepSeek)
  } else {
    // 根据订阅计划选择默认模型
    let workspace = database::workspace::select_workspace(&state.pg_pool, &workspace_id).await?;
    let plan = SubscriptionPlan::try_from(workspace.workspace_type)
      .map_err(|e| AppError::Internal(anyhow::anyhow!("Invalid workspace type: {}", e)))?;
    
    match plan {
      SubscriptionPlan::Free | SubscriptionPlan::Basic => AIModel::DeepSeek,
      SubscriptionPlan::Pro | SubscriptionPlan::Team | SubscriptionPlan::AiMax => AIModel::Qwen3VlPlus,
      _ => AIModel::DeepSeek,
    }
  };
  
  trace!("Using AI model: {:?}", model);
  
  // 3. 检查模型是否可用
  if !state.chat_client.is_model_available(model) {
    error!("AI model {:?} is not available (API key not configured)", model);
    return Ok(
      HttpResponse::ServiceUnavailable()
        .json(serde_json::json!({
          "code": "MODEL_UNAVAILABLE",
          "message": format!("AI model {:?} is not available", model),
        }))
    );
  }
  
  // 4. 如果有图片，根据模型决定处理方式
  let mut params = params;
  if params.has_images && params.images.is_some() {
    match model {
      AIModel::Qwen3VlPlus => {
        info!("📸 [stream_chat] 通义千问支持 base64 data URL，跳过对象存储上传");
      },
      AIModel::Doubao => {
        if let Some(qiniu_client) = &state.qiniu_client {
          let mut image_urls = Vec::new();
          if let Some(images) = &params.images {
            for (idx, image_data) in images.iter().enumerate() {
              if image_data.starts_with("http://") || image_data.starts_with("https://") {
                image_urls.push(image_data.clone());
              } else {
                match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, image_data) {
                  Ok(image_bytes) => {
                    let timestamp = std::time::SystemTime::now()
                      .duration_since(std::time::UNIX_EPOCH)
                      .unwrap()
                      .as_millis();
                    let object_key = format!(
                      "ai-chat-images/{}/{}-{}.jpg",
                      workspace_id.to_string(),
                      timestamp,
                      idx
                    );
                    match qiniu_client.upload_file(&object_key, image_bytes, "image/jpeg").await {
                      Ok(url) => {
                        info!("Image {} uploaded to Qiniu Cloud: {}", idx, url);
                        image_urls.push(url);
                      },
                      Err(e) => {
                        error!("Failed to upload image {} to Qiniu Cloud: {}", idx, e);
                        image_urls.push(image_data.clone());
                      }
                    }
                  },
                  Err(e) => {
                    error!("Failed to decode base64 image {}: {}", idx, e);
                  }
                }
              }
            }
          }
          if !image_urls.is_empty() {
            params.images = Some(image_urls);
          }
        } else {
          warn!("Qiniu Cloud not configured, Doubao requires image URLs");
        }
      },
      AIModel::DeepSeek => {
        info!("⚠️ [stream_chat] DeepSeek 不支持多模态图片");
      },
    }
  }
  
  // 5. 调用 ChatClient 进行流式聊天
  match state.chat_client.stream_chat(&params, model).await {
    Ok(stream) => {
      // 6. 异步增加 AI 使用量
      let pg_pool = state.pg_pool.clone();
      let ws_id = workspace_id;
      tokio::spawn(async move {
        if let Err(e) = increment_ai_usage(&pg_pool, &ws_id).await {
          error!("Failed to increment AI usage: {:?}", e);
        }
      });
      
      // 7. 返回流式响应
      Ok(
        HttpResponse::Ok()
          .content_type("text/event-stream")
          .streaming(stream.map(|result| {
            result.map_err(|e| actix_web::error::ErrorInternalServerError(e))
          }))
      )
    },
    Err(err) => {
      error!("AI chat service error: {:?}", err);
      Ok(
        HttpResponse::InternalServerError()
          .json(serde_json::json!({
            "code": "AI_SERVICE_ERROR",
            "message": format!("AI service error: {}", err),
          }))
      )
    },
  }
}

/// 获取可用的AI模型列表 (公开接口，无需认证，无需参数)
#[instrument(level = "debug", skip(state), err)]
async fn list_chat_models_handler(
  state: Data<AppState>,
) -> actix_web::Result<Json<AppResponse<AvailableModelsResponse>>> {
  trace!("List all available chat models (public endpoint, no auth required)");
  
  // 获取所有实际配置的可用模型
  let available_models = state.chat_client.get_available_models();
  
  trace!("Available models from ChatClient: {:?}", available_models);
  
  // 返回所有配置好的模型，不基于订阅计划
  let mut models = vec![];
  
  if available_models.contains(&AIModel::DeepSeek) {
    models.push(AIModelInfo {
      id: "deepseek-chat".to_string(),
      name: "DeepSeek".to_string(),
      description: "高性能对话模型".to_string(),
      is_default: true,
    });
  }
  
  if available_models.contains(&AIModel::Qwen3VlPlus) {
    models.push(AIModelInfo {
      id: "qwen3-vl-plus".to_string(),
      name: "通义千问".to_string(),
      description: "阿里云通义千问qwen3".to_string(),
      is_default: false,
    });
  }
  
  if available_models.contains(&AIModel::Doubao) {
    models.push(AIModelInfo {
      id: "doubao".to_string(),
      name: "豆包".to_string(),
      description: "字节跳动豆包".to_string(),
      is_default: false,
    });
  }
  
  Ok(AppResponse::Ok().with_data(AvailableModelsResponse {
    models,
    current_plan: "public".to_string(), // 公开接口，无订阅计划
  }).into())
}

/// 检查用户剩余AI调用次数（按uid检查）
async fn check_ai_remaining_by_uid(
  state: &AppState,
  uid: i64,
) -> Result<i64, AppError> {
  // 获取用户当前订阅信息
  let subscription_response = fetch_current_subscription(&state.pg_pool, uid).await?;
  
  // 获取剩余次数
  let remaining = subscription_response.usage.ai_chat_remaining_this_month;
  
  // 如果为 None，表示无限制，返回最大值；否则返回剩余次数
  match remaining {
    Some(count) => Ok(count),
    None => Ok(i64::MAX), // 无限制
  }
}

/// 根据workspace_id获取资源应消耗的目标用户uid
/// 协作区场景：消耗workspace owner的资源配额
async fn get_resource_owner_uid(
  state: &AppState,
  user_uuid: &UserUuid,
  workspace_id_str: Option<&str>,
) -> Result<i64, AppError> {
  if let Some(ws_id_str) = workspace_id_str {
    // 解析workspace_id
    if let Ok(ws_id) = Uuid::parse_str(ws_id_str) {
      // 获取workspace信息，找到owner_uid
      match database::workspace::select_workspace(&state.pg_pool, &ws_id).await {
        Ok(workspace) => {
          if let Some(owner_uid) = workspace.owner_uid {
            info!(
              "🔍 [资源归属] workspace_id: {}, owner_uid: {}, 协作区资源消耗归属workspace owner",
              ws_id, owner_uid
            );
            return Ok(owner_uid);
          }
        },
        Err(e) => {
          warn!("⚠️ [资源归属] 查询workspace失败: {:?}, 回退为使用当前用户", e);
        }
      }
    }
  }
  
  // 回退: 使用当前请求用户
  let uid = state.user_cache.get_user_uid(user_uuid).await?;
  info!("🔍 [资源归属] 未提供workspace_id或查询失败，使用当前用户 uid: {}", uid);
  Ok(uid)
}

/// 记录AI聊天使用情况（按uid记录）
async fn record_ai_chat_usage_by_uid(
  state: &AppState,
  uid: i64,
) -> Result<(), AppError> {
  record_usage(
    &state.pg_pool,
    uid,
    UsageRecordRequest {
      usage_type: UsageType::AiChat,
      usage_count: 1,
      usage_date: None, // 使用当前日期
    },
  )
  .await?;
  
  Ok(())
}

/// 聊天会话接口 (需要JWT认证，根据订阅计划限制使用次数)
/// 协作区场景：当用户B在用户A的协作区使用AI时，消耗A的AI配额
#[instrument(level = "debug", skip(state, payload, user_uuid), err)]
async fn public_chat_session_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  payload: Json<ChatRequestParams>,
) -> actix_web::Result<HttpResponse> {
  let params = payload.into_inner();
  
  info!(
    "🔍 [AI会话] 收到用户请求 - user: {}, message_len: {}, model: {:?}, workspace_id: {:?}", 
    *user_uuid,
    params.message.len(),
    params.preferred_model,
    params.workspace_id
  );
  info!(
    "🔍 [AI会话] 请求参数 - has_images: {}, images_count: {}, has_files: {}, thinking: {}, search: {}", 
    params.has_images,
    params.images.as_ref().map(|v| v.len()).unwrap_or(0),
    params.has_files,
    params.enable_thinking,
    params.enable_web_search
  );
  
  // 1. 确定资源消耗的目标用户（协作区场景：消耗workspace owner的配额）
  let resource_owner_uid = match get_resource_owner_uid(
    &state,
    &user_uuid,
    params.workspace_id.as_deref(),
  ).await {
    Ok(uid) => uid,
    Err(err) => {
      error!("Failed to determine resource owner: {:?}", err);
      // 回退为使用当前用户
      state.user_cache.get_user_uid(&user_uuid).await
        .map_err(|e| actix_web::error::ErrorInternalServerError(e))?
    }
  };
  
  info!(
    "🔍 [AI会话] 资源消耗归属 - resource_owner_uid: {}, calling_user: {}", 
    resource_owner_uid, *user_uuid
  );
  
  // 2. 检查资源owner的剩余AI调用次数
  let remaining = match check_ai_remaining_by_uid(&state, resource_owner_uid).await {
    Ok(count) => count,
    Err(AppError::RecordNotFound(_)) => {
      error!("Resource owner uid {} has no active subscription", resource_owner_uid);
      return Ok(
        HttpResponse::NotFound()
          .json(serde_json::json!({
            "code": "SUBSCRIPTION_NOT_FOUND",
            "message": "抱歉，该工作空间的所有者还未开启订阅计划，问AI功能暂时不可用。",
          }))
      );
    },
    Err(err) => {
      error!("Failed to check AI remaining for resource owner uid {}: {:?}", resource_owner_uid, err);
      return Ok(
        HttpResponse::InternalServerError()
          .json(serde_json::json!({
            "code": "INTERNAL_ERROR",
            "message": format!("检查订阅信息失败: {}", err),
          }))
      );
    },
  };
  
  // 3. 检查剩余次数是否足够（>= 1）
  if remaining < 1 {
    error!("Resource owner uid {} AI limit exceeded, remaining: {}", resource_owner_uid, remaining);
    return Ok(
      HttpResponse::PaymentRequired()
        .json(serde_json::json!({
          "code": "AI_LIMIT_EXCEEDED",
          "message": format!("AI调用次数已用完，剩余次数：{}，请升级订阅或购买补充包", remaining),
        }))
    );
  }
  
  trace!("Resource owner uid {} has {} remaining AI calls", resource_owner_uid, remaining);
  
  // 3. 确定使用的模型（必须指定，或使用默认的 DeepSeek）
  let model = if let Some(model_id) = &params.preferred_model {
    AIModel::from_str(model_id).unwrap_or(AIModel::DeepSeek)
  } else {
    AIModel::DeepSeek // 默认使用 DeepSeek
  };
  
  trace!("Using AI model: {:?}", model);
  
  // 4. 检查模型是否可用
  if !state.chat_client.is_model_available(model) {
    error!("AI model {:?} is not available (API key not configured)", model);
    return Ok(
      HttpResponse::ServiceUnavailable()
        .json(serde_json::json!({
          "code": "MODEL_UNAVAILABLE",
          "message": format!("AI model {:?} is not available", model),
        }))
    );
  }
  
  // 5. 如果有图片，根据模型决定处理方式
  //    - 通义千问(Qwen)：支持 base64 data URL，直接保留 base64 数据即可
  //    - 豆包(Doubao)：只支持 HTTP(S) URL，需要上传到对象存储
  //    - DeepSeek：不支持多模态，跳过图片处理
  let mut params = params;
  if params.has_images && params.images.is_some() {
    info!("📸 [AI会话] 检测到图片数据，模型: {:?}，准备处理...", model);
    
    match model {
      AIModel::Qwen3VlPlus => {
        // 通义千问支持 OpenAI 兼容格式的 base64 data URL，不需要上传到对象存储
        // build_messages_for_openai_compatible 会自动添加 "data:image/jpeg;base64," 前缀
        info!("📸 [AI会话] 通义千问支持 base64 data URL，跳过对象存储上传，直接使用 base64 数据");
        let image_count = params.images.as_ref().map(|v| v.len()).unwrap_or(0);
        info!("📸 [AI会话] 保留 {} 张图片的 base64 数据", image_count);
      },
      AIModel::Doubao => {
        // 豆包只支持 HTTP(S) URL，需要上传到七牛云
        info!("📸 [AI会话] 豆包需要 URL 格式，准备上传到七牛云");
        if let Some(qiniu_client) = &state.qiniu_client {
          info!("✅ [AI会话] 七牛云客户端已配置，开始上传图片");
          let mut image_urls = Vec::new();
          
          if let Some(images) = &params.images {
            for (idx, image_data) in images.iter().enumerate() {
              if image_data.starts_with("http://") || image_data.starts_with("https://") {
                image_urls.push(image_data.clone());
              } else {
                match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, image_data) {
                  Ok(image_bytes) => {
                    let timestamp = std::time::SystemTime::now()
                      .duration_since(std::time::UNIX_EPOCH)
                      .unwrap()
                      .as_millis();
                    let object_key = format!(
                      "ai-chat-images/{}/{}-{}.jpg",
                      user_uuid.to_string(),
                      timestamp,
                      idx
                    );
                    
                    info!("🔄 [AI会话] 开始上传图片 {} 到七牛云，key: {}", idx, object_key);
                    
                    match qiniu_client.upload_file(&object_key, image_bytes, "image/jpeg").await {
                      Ok(url) => {
                        info!("✅ [AI会话] 图片 {} 上传成功！URL: {}", idx, url);
                        image_urls.push(url);
                      },
                      Err(e) => {
                        error!("❌ [AI会话] 图片 {} 上传失败: {}", idx, e);
                        image_urls.push(image_data.clone());
                      }
                    }
                  },
                  Err(e) => {
                    error!("❌ [AI会话] 图片 {} base64解码失败: {}", idx, e);
                  }
                }
              }
            }
          }
          
          if !image_urls.is_empty() {
            info!("✅ [AI会话] 图片处理完成，共 {} 张图片转换为URL", image_urls.len());
            for (i, url) in image_urls.iter().enumerate() {
              info!("   图片 {}: {}", i, url);
            }
            params.images = Some(image_urls);
          } else {
            error!("❌ [AI会话] 图片处理后没有有效的图片数据");
          }
        } else {
          error!("❌ [AI会话] 七牛云客户端未配置！豆包模型需要图片URL，无法处理");
          error!("   请检查环境变量：QINIU_ENABLED, QINIU_ACCESS_KEY, QINIU_SECRET_KEY");
        }
      },
      AIModel::DeepSeek => {
        info!("⚠️ [AI会话] DeepSeek 不支持多模态图片，图片数据将被忽略");
      },
    }
  } else {
    info!("ℹ️ [AI会话] 本次请求不包含图片");
  }
  
  // 6. 调用 ChatClient 进行流式聊天
  match state.chat_client.stream_chat(&params, model).await {
    Ok(stream) => {
      // 7. 异步记录使用情况（记录在workspace owner名下，不阻塞响应）
      let state_clone = state.clone();
      let resource_owner_uid_clone = resource_owner_uid;
      tokio::spawn(async move {
        if let Err(e) = record_ai_chat_usage_by_uid(&state_clone, resource_owner_uid_clone).await {
          error!("Failed to record AI chat usage for resource owner uid {}: {:?}", resource_owner_uid_clone, e);
        }
      });
      
      // 8. 返回流式响应
      Ok(
        HttpResponse::Ok()
          .content_type("text/event-stream")
          .streaming(stream.map(|result| {
            result.map_err(|e| actix_web::error::ErrorInternalServerError(e))
          }))
      )
    },
    Err(err) => {
      error!("AI chat service error: {:?}", err);
      Ok(
        HttpResponse::InternalServerError()
          .json(serde_json::json!({
            "code": "AI_SERVICE_ERROR",
            "message": format!("AI service error: {}", err),
          }))
      )
    },
  }
}

/// 文件上传处理接口（需要JWT认证）
/// 支持多种文件类型的上传和预处理
#[instrument(level = "debug", skip(state, payload, user_uuid), err)]
async fn upload_file_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  mut payload: actix_multipart::Multipart,
) -> actix_web::Result<HttpResponse> {
  use futures_util::StreamExt;
  
  trace!("File upload request from user {}", *user_uuid);
  
  let mut files_processed = Vec::new();
  
  while let Some(item) = payload.next().await {
    let mut field = item.map_err(|e| {
      error!("Failed to read multipart field: {}", e);
      actix_web::error::ErrorBadRequest(format!("Invalid multipart data: {}", e))
    })?;
    
    let filename = field
      .content_disposition()
      .and_then(|cd| cd.get_filename())
      .unwrap_or("unnamed_file")
      .to_string();
    
    trace!("Processing file: {}", filename);
    
    // 读取文件数据
    let mut file_data = Vec::new();
    while let Some(chunk) = field.next().await {
      let data = chunk.map_err(|e| {
        error!("Failed to read file chunk: {}", e);
        actix_web::error::ErrorBadRequest(format!("Failed to read file: {}", e))
      })?;
      file_data.extend_from_slice(&data);
    }
    
    let file_size = file_data.len() as i64;
    
    // 硬编码上限: 20MB
    if file_size > 20 * 1024 * 1024 {
      return Ok(
        HttpResponse::PayloadTooLarge()
          .json(serde_json::json!({
            "code": "FILE_TOO_LARGE",
            "message": format!("文件 {} 超过20MB限制", filename),
          }))
      );
    }

    // 订阅计划验证：获取用户uid并检查单文件大小限制和总存储空间
    let uid = state.user_cache.get_user_uid(&user_uuid).await.map_err(|e| {
      error!("获取用户UID失败: {}", e);
      actix_web::error::ErrorInternalServerError("用户信息获取失败")
    })?;

    let resource_status = get_user_resource_limit_status(&state.pg_pool, uid).await.map_err(|e| {
      error!("获取用户资源限制状态失败: {}", e);
      actix_web::error::ErrorInternalServerError("获取用户订阅信息失败")
    })?;

    let plan = SubscriptionPlan::try_from(resource_status.plan_code.as_str())
      .unwrap_or(SubscriptionPlan::Free);
    let plan_limits = PlanLimits::from_plan(&plan);

    // 检查单文件大小限制
    if file_size > plan_limits.single_upload_limit {
      return Ok(
        HttpResponse::PayloadTooLarge()
          .json(serde_json::json!({
            "code": "SINGLE_FILE_LIMIT_EXCEEDED",
            "message": format!(
              "文件 {} ({}MB) 超过当前套餐({})的单文件上传限制({}MB)",
              filename,
              file_size / (1024 * 1024),
              resource_status.plan_code,
              plan_limits.single_upload_limit / (1024 * 1024)
            ),
          }))
      );
    }

    // 检查总存储空间限制
    let current_usage = get_user_total_usage_bytes(&state.pg_pool, uid).await.map_err(|e| {
      error!("获取用户存储用量失败: {}", e);
      actix_web::error::ErrorInternalServerError("获取用户存储用量失败")
    })? as i64;
    let total_limit_bytes = (resource_status.storage_limit_mb * 1024.0 * 1024.0) as i64;

    if current_usage + file_size > total_limit_bytes {
      return Ok(
        HttpResponse::PayloadTooLarge()
          .json(serde_json::json!({
            "code": "STORAGE_LIMIT_EXCEEDED",
            "message": format!(
              "云存储空间不足。当前已用: {}MB, 总限额: {}MB, 本次上传: {}MB (套餐: {})",
              current_usage / (1024 * 1024),
              total_limit_bytes / (1024 * 1024),
              file_size / (1024 * 1024),
              resource_status.plan_code
            ),
          }))
      );
    }

    info!(
      "📤 [AI文件上传] 验证通过 - 用户uid: {}, 文件: {}, 大小: {}B, 套餐: {}, 已用: {}MB/{}MB",
      uid, filename, file_size, resource_status.plan_code,
      current_usage / (1024 * 1024), total_limit_bytes / (1024 * 1024)
    );
    
    // 提取文件类型
    let file_type = filename
      .split('.')
      .last()
      .unwrap_or("unknown")
      .to_lowercase();
    
    trace!("File type: {}, size: {} bytes", file_type, file_size);
    
    // 尝试上传到七牛云（如果已启用）
    let file_url = if let Some(qiniu_client) = &state.qiniu_client {
      trace!("Uploading file to Qiniu Cloud: {}", filename);
      
      // 推断Content-Type
      let content_type = infra::qiniu_client::infer_content_type(&filename);
      
      // 生成对象存储key
      let object_key = infra::qiniu_client::QiniuClient::generate_ai_file_key(
        "ai-session",  // 暂时使用固定workspace_id，后续可以改为实际的
        &user_uuid.to_string(),
        &filename,
      );
      
      // 上传到七牛云
      match qiniu_client.upload_file(&object_key, file_data.clone(), &content_type).await {
        Ok(url) => {
          info!("File uploaded to Qiniu Cloud successfully: {}", url);
          Some(url)
        },
        Err(e) => {
          error!("Failed to upload file to Qiniu Cloud: {}", e);
          None  // 失败时返回None，后续可以fallback到base64
        }
      }
    } else {
      trace!("Qiniu Cloud not enabled, file will be processed locally");
      None
    };
    
    // 根据文件类型进行预处理
    let (content_type, content_data) = match file_type.as_str() {
      // 文本文件：直接转换为文本
      "txt" | "md" | "markdown" | "json" | "xml" | "html" | "css" | "js" 
      | "ts" | "jsx" | "tsx" | "py" | "rs" | "go" | "java" | "c" | "cpp" 
      | "h" | "hpp" | "sh" | "bash" | "yaml" | "yml" | "toml" | "ini" 
      | "log" | "csv" | "sql" => {
        match String::from_utf8(file_data.clone()) {
          Ok(text) => {
            info!("Text file processed: {} ({} chars)", filename, text.len());
            ("text", Some(text))
          },
          Err(_) => ("base64", Some(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &file_data)))
        }
      },
      // 图片文件：如果有URL则使用URL，否则使用base64
      "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "svg" => {
        if file_url.is_some() {
          ("url", None)  // 如果有URL，不需要传输文件内容
        } else {
          ("base64", Some(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &file_data)))
        }
      },
      // PDF和Office文档：优先使用URL
      "pdf" | "docx" | "xlsx" | "pptx" => {
        if file_url.is_some() {
          warn!("Document {} uploaded to Qiniu Cloud", filename);
          ("url", None)
        } else {
          warn!("Document {} will be sent as base64 (Qiniu not available)", filename);
          ("base64", Some(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &file_data)))
        }
      },
      _ => {
        if file_url.is_some() {
          ("url", None)
        } else {
          ("base64", Some(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &file_data)))
        }
      }
    };
    
    let mut file_info = serde_json::json!({
      "file_name": filename,
      "file_type": file_type,
      "file_size": file_size,
      "content_type": content_type,
      "status": "processed",
    });
    
    // 添加URL或content
    if let Some(url) = file_url {
      file_info["url"] = serde_json::json!(url);
    } else if let Some(content) = content_data {
      file_info["content"] = serde_json::json!(content);
    }
    
    files_processed.push(file_info);
  }
  
  info!("Successfully processed {} file(s)", files_processed.len());
  
  Ok(
    HttpResponse::Ok()
      .json(serde_json::json!({
        "code": "SUCCESS",
        "message": "文件上传成功",
        "files": files_processed,
      }))
  )
}
