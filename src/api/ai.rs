use crate::api::util::ai_model_from_header;
use crate::biz::authentication::jwt::UserUuid;
use crate::biz::workspace::subscription_plan_limits::PlanLimits;
use crate::state::AppState;

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

use tracing::{error, instrument, trace};

pub fn ai_completion_scope() -> Scope {
  web::scope("/api/ai")
    // 公开接口（无需认证，无需workspace_id）
    .service(web::resource("/chat/models").route(web::get().to(list_chat_models_handler)))
    .service(web::resource("/chat/session").route(web::post().to(public_chat_session_handler)))
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
  
  // 4. 调用 ChatClient 进行流式聊天
  match state.chat_client.stream_chat(&params, model).await {
    Ok(stream) => {
      // 5. 异步增加 AI 使用量
      let pg_pool = state.pg_pool.clone();
      let ws_id = workspace_id;
      tokio::spawn(async move {
        if let Err(e) = increment_ai_usage(&pg_pool, &ws_id).await {
          error!("Failed to increment AI usage: {:?}", e);
        }
      });
      
      // 6. 返回流式响应
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

/// 公开的聊天会话接口 (无需认证，无需workspace_id，无使用限额)
#[instrument(level = "debug", skip(state, payload), err)]
async fn public_chat_session_handler(
  state: Data<AppState>,
  payload: Json<ChatRequestParams>,
) -> actix_web::Result<HttpResponse> {
  let params = payload.into_inner();
  
  trace!(
    "Public chat request, message length: {}, model: {:?}", 
    params.message.len(),
    params.preferred_model
  );
  
  // 1. 确定使用的模型（必须指定，或使用默认的 DeepSeek）
  let model = if let Some(model_id) = &params.preferred_model {
    AIModel::from_str(model_id).unwrap_or(AIModel::DeepSeek)
  } else {
    AIModel::DeepSeek // 公开接口默认使用 DeepSeek
  };
  
  trace!("Using AI model: {:?}", model);
  
  // 2. 检查模型是否可用
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
  
  // 3. 调用 ChatClient 进行流式聊天
  match state.chat_client.stream_chat(&params, model).await {
    Ok(stream) => {
      // 4. 返回流式响应
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
