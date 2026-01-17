use crate::api::util::ai_model_from_header;
use crate::biz::authentication::jwt::UserUuid;
use crate::biz::subscription::ops::{fetch_current_subscription, record_usage};
use crate::biz::workspace::subscription_plan_limits::PlanLimits;
use crate::state::AppState;
use shared_entity::dto::subscription_dto::UsageRecordRequest;
use shared_entity::dto::subscription_dto::UsageType;

use actix_web::web::{Data, Json};
use actix_web::{web, HttpRequest, HttpResponse, Scope};
use app_error::AppError;
use appflowy_ai_client::dto::{
  CalculateSimilarityParams, LocalAIConfig, ModelList, SimilarityResponse, TranslateRowParams,
  TranslateRowResponse,
};
use appflowy_ai_client::{AIModel, AIModelInfo, AvailableModelsResponse, ChatRequestParams};

use futures_util::{stream, StreamExt, TryStreamExt};

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
      SubscriptionPlan::Pro => AIModel::QwenTurbo,
      SubscriptionPlan::Team | SubscriptionPlan::AiMax => AIModel::QwenMax,
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
  
  // 4. 如果有图片，上传到七牛云（豆包需要URL格式）
  let mut params = params;
  if params.has_images && params.images.is_some() {
    if let Some(qiniu_client) = &state.qiniu_client {
      let mut image_urls = Vec::new();
      
      if let Some(images) = &params.images {
        for (idx, image_data) in images.iter().enumerate() {
          // 判断是否已经是URL
          if image_data.starts_with("http://") || image_data.starts_with("https://") {
            // 已经是URL，直接使用
            image_urls.push(image_data.clone());
          } else {
            // 是base64数据，需要上传到七牛云
            match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, image_data) {
              Ok(image_bytes) => {
                // 生成对象存储key
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
                
                // 上传到七牛云
                match qiniu_client.upload_file(&object_key, image_bytes, "image/jpeg").await {
                  Ok(url) => {
                    info!("Image {} uploaded to Qiniu Cloud: {}", idx, url);
                    image_urls.push(url);
                  },
                  Err(e) => {
                    error!("Failed to upload image {} to Qiniu Cloud: {}", idx, e);
                    // 如果上传失败，保留base64（对于不需要URL的模型）
                    image_urls.push(image_data.clone());
                  }
                }
              },
              Err(e) => {
                error!("Failed to decode base64 image {}: {}", idx, e);
                // 解码失败，跳过此图片
              }
            }
          }
        }
      }
      
      // 更新params中的图片数据为URL
      if !image_urls.is_empty() {
        params.images = Some(image_urls);
        trace!("Updated {} image(s) to URL format", params.images.as_ref().unwrap().len());
      } else {
        warn!("No valid images after processing");
      }
    } else {
      warn!("Qiniu Cloud not configured, images will be sent as base64 (may not work with Doubao)");
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
  
  if available_models.contains(&AIModel::QwenTurbo) {
    models.push(AIModelInfo {
      id: "qwen-turbo".to_string(),
      name: "通义千问 Turbo".to_string(),
      description: "阿里云通义千问快速版".to_string(),
      is_default: false,
    });
  }
  
  if available_models.contains(&AIModel::QwenMax) {
    models.push(AIModelInfo {
      id: "qwen-max".to_string(),
      name: "通义千问 Max".to_string(),
      description: "阿里云通义千问旗舰版".to_string(),
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

/// 检查用户剩余AI调用次数
async fn check_user_ai_remaining(
  state: &AppState,
  user_uuid: &UserUuid,
) -> Result<i64, AppError> {
  // 1. 获取用户 uid
  let uid = state.user_cache.get_user_uid(user_uuid).await?;
  
  // 2. 获取用户当前订阅信息
  let subscription_response = fetch_current_subscription(&state.pg_pool, uid).await?;
  
  // 3. 获取剩余次数
  let remaining = subscription_response.usage.ai_chat_remaining_this_month;
  
  // 4. 如果为 None，表示无限制，返回最大值；否则返回剩余次数
  match remaining {
    Some(count) => Ok(count),
    None => Ok(i64::MAX), // 无限制
  }
}

/// 记录AI聊天使用情况
async fn record_ai_chat_usage(
  state: &AppState,
  user_uuid: &UserUuid,
) -> Result<(), AppError> {
  let uid = state.user_cache.get_user_uid(user_uuid).await?;
  
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
#[instrument(level = "debug", skip(state, payload, user_uuid), err)]
async fn public_chat_session_handler(
  user_uuid: UserUuid,
  state: Data<AppState>,
  payload: Json<ChatRequestParams>,
) -> actix_web::Result<HttpResponse> {
  let params = payload.into_inner();
  
  info!(
    "🔍 [AI会话] 收到用户请求 - user: {}, message_len: {}, model: {:?}", 
    *user_uuid,
    params.message.len(),
    params.preferred_model
  );
  info!(
    "🔍 [AI会话] 请求参数 - has_images: {}, images_count: {}, has_files: {}, thinking: {}, search: {}", 
    params.has_images,
    params.images.as_ref().map(|v| v.len()).unwrap_or(0),
    params.has_files,
    params.enable_thinking,
    params.enable_web_search
  );
  
  // 1. 检查用户剩余AI调用次数
  let remaining = match check_user_ai_remaining(&state, &user_uuid).await {
    Ok(count) => count,
    Err(AppError::RecordNotFound(_)) => {
      error!("User {} has no active subscription", *user_uuid);
      return Ok(
        HttpResponse::NotFound()
          .json(serde_json::json!({
            "code": "SUBSCRIPTION_NOT_FOUND",
            "message": "抱歉，您还未开启订阅计划，问AI功能暂时不可用。",
          }))
      );
    },
    Err(err) => {
      error!("Failed to check AI remaining for user {}: {:?}", *user_uuid, err);
      return Ok(
        HttpResponse::InternalServerError()
          .json(serde_json::json!({
            "code": "INTERNAL_ERROR",
            "message": format!("检查订阅信息失败: {}", err),
          }))
      );
    },
  };
  
  // 2. 检查剩余次数是否足够（>= 1）
  if remaining < 1 {
    error!("User {} AI limit exceeded, remaining: {}", *user_uuid, remaining);
    return Ok(
      HttpResponse::PaymentRequired()
        .json(serde_json::json!({
          "code": "AI_LIMIT_EXCEEDED",
          "message": format!("AI调用次数已用完，剩余次数：{}，请升级订阅或购买补充包", remaining),
        }))
    );
  }
  
  trace!("User {} has {} remaining AI calls", *user_uuid, remaining);
  
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
  
  // 5. 如果有图片，上传到七牛云（豆包需要URL格式）
  let mut params = params;
  if params.has_images && params.images.is_some() {
    info!("📸 [AI会话] 检测到图片数据，准备处理...");
    if let Some(qiniu_client) = &state.qiniu_client {
      info!("✅ [AI会话] 七牛云客户端已配置，开始上传图片");
      let mut image_urls = Vec::new();
      
      if let Some(images) = &params.images {
        for (idx, image_data) in images.iter().enumerate() {
          // 判断是否已经是URL
          if image_data.starts_with("http://") || image_data.starts_with("https://") {
            // 已经是URL，直接使用
            image_urls.push(image_data.clone());
          } else {
            // 是base64数据，需要上传到七牛云
            match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, image_data) {
              Ok(image_bytes) => {
                // 生成对象存储key
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
                
                // 上传到七牛云
                match qiniu_client.upload_file(&object_key, image_bytes, "image/jpeg").await {
                  Ok(url) => {
                    info!("✅ [AI会话] 图片 {} 上传成功！URL: {}", idx, url);
                    image_urls.push(url);
                  },
                  Err(e) => {
                    error!("❌ [AI会话] 图片 {} 上传失败: {}", idx, e);
                    // 如果上传失败，保留base64（对于不需要URL的模型）
                    image_urls.push(image_data.clone());
                  }
                }
              },
              Err(e) => {
                error!("❌ [AI会话] 图片 {} base64解码失败: {}", idx, e);
                // 解码失败，跳过此图片
              }
            }
          }
        }
      }
      
      // 更新params中的图片数据为URL
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
      error!("❌ [AI会话] 七牛云客户端未配置！无法上传图片");
      error!("   请检查环境变量：QINIU_ENABLED, QINIU_ACCESS_KEY, QINIU_SECRET_KEY");
    }
  } else {
    info!("ℹ️ [AI会话] 本次请求不包含图片");
  }
  
  // 6. 调用 ChatClient 进行流式聊天
  match state.chat_client.stream_chat(&params, model).await {
    Ok(stream) => {
      // 7. 异步记录使用情况（不阻塞响应）
      let state_clone = state.clone();
      let user_uuid_clone = user_uuid.clone();
      tokio::spawn(async move {
        if let Err(e) = record_ai_chat_usage(&state_clone, &user_uuid_clone).await {
          error!("Failed to record AI chat usage for user {}: {:?}", *user_uuid_clone, e);
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
    
    // 文件大小限制: 20MB
    if file_size > 20 * 1024 * 1024 {
      return Ok(
        HttpResponse::PayloadTooLarge()
          .json(serde_json::json!({
            "code": "FILE_TOO_LARGE",
            "message": format!("文件 {} 超过20MB限制", filename),
          }))
      );
    }
    
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
