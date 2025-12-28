use actix_web::{web::{Data, Path}, HttpResponse, Result, HttpRequest};
use crate::state::AppState;
use crate::biz::authentication::jwt::OptionalUserUuid;
use crate::biz::workspace::invite::{join_workspace_invite_by_code};
use database::workspace::{select_invited_workspace_id, select_workspace};

/// Short invite URL handler.
/// - If user is authenticated: join workspace by code and redirect to workspace page.
/// - If not authenticated: render a simple landing HTML page with Open App / Download / Accept in Web options.
pub async fn invite_landing_handler(
  optional_user: OptionalUserUuid,
  path: Path<String>,
  state: Data<AppState>,
  _req: HttpRequest,
) -> Result<HttpResponse> {
  let code = path.into_inner();

  // Try to resolve workspace id for display/redirect
  let workspace_id_res = select_invited_workspace_id(&state.pg_pool, &code).await;
  let workspace_id = match workspace_id_res {
    Ok(id) => id,
    Err(_) => {
      return Ok(HttpResponse::NotFound()
        .content_type("text/plain; charset=utf-8")
        .body("Invalid or expired invitation code"));
    }
  };

  if let Some(user_uuid) = optional_user.as_uuid() {
    // logged in, perform join
    let uid = state.user_cache.get_user_uid(&user_uuid).await.map_err(|e| {
      actix_web::error::ErrorInternalServerError(format!("Failed to lookup uid: {}", e))
    })?;
    // perform join
    match join_workspace_invite_by_code(&state.pg_pool, &code, uid) .await {
      Ok(joined_workspace_id) => {
        // redirect to app web workspace page
        let target = format!("{}/app/{}", state.config.appflowy_web_url, joined_workspace_id);
        return Ok(HttpResponse::Found().header("Location", target).finish());
      }
      Err(err) => {
        // cannot join, show error and landing page
        log::error!("Failed to join by invite code: {:?}", err);
      }
    }
  }

  // Not logged in (or join failed) — render simple landing page
  let workspace = select_workspace(&state.pg_pool, &workspace_id).await.ok();
  let workspace_name = workspace.as_ref().and_then(|w| w.workspace_name.clone()).unwrap_or_else(|| "Workspace".to_string());

  // Deep link to app
  let app_deep_link = format!("ponynotes://invitation-callback?workspace_id={}&code={}", workspace_id, code);
  // web accept URL (will point to join API after login)
  let accept_web = format!("{}/app/invited/{}", state.config.appflowy_web_url, code);

  let html = format!(r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8"/>
    <meta name="viewport" content="width=device-width,initial-scale=1"/>
    <title>Invitation to {workspace_name}</title>
    <style>
      body {{ font-family: Arial, Helvetica, sans-serif; background:#fafafa; color:#222; }}
      .card {{ max-width:720px;margin:48px auto;padding:24px;border-radius:8px;background:#fff;box-shadow:0 2px 10px rgba(0,0,0,0.08); }}
      .btn {{ display:inline-block;padding:10px 16px;border-radius:6px;text-decoration:none;color:#fff;background:#007bff;margin-right:8px; }}
      .muted {{ color:#666;font-size:14px; }}
    </style>
  </head>
  <body>
    <div class="card">
      <h2>你被邀请加入工作空间：{workspace_name}</h2>
      <p class="muted">使用以下方式加入：</p>
      <p>
        <a class="btn" href="{app_deep_link}">在应用中打开</a>
        <a id="accept-web-btn" class="btn" href="{accept_web}">在网页中接受邀请</a>
      </p>
      <p class="muted">如果没有安装应用，请从下面下载：</p>
      <p>
        <a href="{app_store}" target="_blank">下载 iOS App</a> |
        <a href="{play_store}" target="_blank">下载 Android App</a> |
        <a href="{desktop_download}" target="_blank">下载 桌面端</a>
      </p>
      <hr/>
      <p class="muted">邀请代码：<code>{code}</code></p>
    </div>

    <script>
      (function() {{
        // 保存邀请码到 localStorage，供登录后自动处理
        localStorage.setItem('pending_invite_code', '{code}');
        localStorage.setItem('pending_workspace_id', '{workspace_id}');

        // 修改"网页中接受邀请"按钮的行为
        document.getElementById('accept-web-btn').addEventListener('click', function(e) {{
          e.preventDefault();
          // 保存pending状态并跳转到登录页面
          localStorage.setItem('pending_invite_action', 'accept');
          window.location.href = '{appflowy_web_url}/app/#/login';
        }});
      }})();
    </script>
  </body>
</html>"#,
    workspace_name = html_escape::encode_text(&workspace_name),
    app_deep_link = html_escape::encode_text(&app_deep_link),
    accept_web = html_escape::encode_text(&accept_web),
    app_store = state.config.appflowy_web_url.clone(), // placeholder
    play_store = state.config.appflowy_web_url.clone(), // placeholder
    desktop_download = state.config.appflowy_web_url.clone(), // placeholder
    code = html_escape::encode_text(&code),
    workspace_id = workspace_id,
    appflowy_web_url = state.config.appflowy_web_url
  );

  Ok(HttpResponse::Ok().content_type("text/html; charset=utf-8").body(html))
}


