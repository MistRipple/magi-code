use super::*;

pub(super) async fn list_annotations(
    State(state): State<ApiState>,
    Path(tab_id): Path<String>,
) -> Result<Json<Vec<BrowserAnnotationResponse>>, ApiError> {
    let tab_id = BrowserTabId::new(tab_id);
    let (_tab, session) = browser_tab_scope(&state, &tab_id)?;
    require_session_scope(&session)?;
    ensure_browser_ui_ready(&state)?;
    let annotations = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .annotations_for_tab(&tab_id)
        .into_iter()
        .map(BrowserAnnotationResponse::from)
        .collect();
    Ok(Json(annotations))
}

pub(super) async fn create_annotation(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(tab_id): Path<String>,
    Json(request): Json<CreateBrowserAnnotationRequest>,
) -> Result<(StatusCode, Json<BrowserAnnotationResponse>), ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let tab_id = BrowserTabId::new(tab_id);
    let (_tab_before_lock, session) = browser_tab_scope(&state, &tab_id)?;
    let comment = request.comment.trim().to_string();
    if comment.is_empty() || comment.chars().count() > 4_000 {
        return Err(ApiError::InvalidInput(
            "标记内容不能为空且不能超过 4000 个字符".to_string(),
        ));
    }
    // 用户接管可能需要中断当前 Session Turn。该异步操作必须在取得全局
    // Tab 控制锁之前完成，否则会与 close_session 的
    // session_turn_lock -> Tab 控制锁顺序形成反向等待。
    ensure_user_control_for_ui(&state, &session, &tab_id).await?;
    let _control_guard = state.browser_control_locks.lock_tab(&tab_id).await;
    // 获得控制锁后重新读取权威 Tab；请求等待期间可能已完成面板调整或导航。
    let (tab, session) = browser_tab_scope(&state, &tab_id)?;
    let (kind, navigation_revision, hit_x, hit_y, region) = match request.selection {
        BrowserAnnotationSelectionRequest::Element {
            navigation_revision,
            normalized_x,
            normalized_y,
        } => {
            validate_annotation_point(normalized_x, normalized_y)?;
            (
                BrowserAnnotationKind::Element,
                navigation_revision,
                normalized_x,
                normalized_y,
                None,
            )
        }
        BrowserAnnotationSelectionRequest::Region {
            navigation_revision,
            rect,
        } => {
            validate_annotation_rect(rect)?;
            let x = rect.x + rect.width / 2.0;
            let y = rect.y + rect.height / 2.0;
            (
                BrowserAnnotationKind::Region,
                navigation_revision,
                x,
                y,
                Some(rect),
            )
        }
    };
    state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .validate_navigation_ref(&session.browser_session_id, &tab_id, navigation_revision)
        .map_err(|error| ApiError::Conflict(error.to_string()))?;
    let hit = browser_hit_test(&state, &tab_id, navigation_revision, hit_x, hit_y).await?;
    let hit_viewport = BrowserViewport {
        width: hit.viewport_width,
        height: hit.viewport_height,
        device_scale_factor_millis: hit.device_scale_factor_millis,
        device_type: hit.device_type,
    };
    let anchor = match region {
        Some(rect) => BrowserAnnotationAnchor::Region(BrowserRegionAnnotationAnchor {
            url: tab.url.clone(),
            origin: tab.origin.clone(),
            viewport: hit_viewport,
            scroll_x: hit.scroll_x,
            scroll_y: hit.scroll_y,
            rect,
            snapshot_revision: tab.snapshot_revision,
        }),
        None => BrowserAnnotationAnchor::Element(Box::new(BrowserElementAnnotationAnchor {
            url: tab.url.clone(),
            origin: tab.origin.clone(),
            frame_path: Vec::new(),
            viewport: hit_viewport,
            scroll_x: hit.scroll_x,
            scroll_y: hit.scroll_y,
            test_id: hit.test_id,
            stable_id: hit.stable_id,
            aria_role: hit.aria_role,
            aria_name: hit.aria_name,
            tag_name: hit.tag_name,
            text_excerpt: hit.text_excerpt,
            css_path: hit.css_path,
            ancestor_fingerprint: hit.ancestor_fingerprint,
            dom_fingerprint: hit.dom_fingerprint,
            bounding_box: normalize_hit_bounds(
                hit.viewport_width,
                hit.viewport_height,
                hit.bounds,
            )?,
            snapshot_revision: tab.snapshot_revision,
        })),
    };
    let screenshot_clip = match &anchor {
        BrowserAnnotationAnchor::Element(anchor) => anchor.bounding_box,
        BrowserAnnotationAnchor::Region(anchor) => anchor.rect,
    };
    let screenshot_artifact_id = persist_browser_annotation_screenshot(
        &state,
        require_session_scope(&session)?,
        &tab.tab_id,
        screenshot_clip,
        &tab_id,
        hit.navigation_revision,
    )
    .await?;
    let artifact_id_for_cleanup = screenshot_artifact_id.clone();
    let now = UtcMillis::now();
    let annotation = match state.mutate_browser_authority(|authority| {
        authority.validate_navigation_ref(
            &session.browser_session_id,
            &tab_id,
            navigation_revision,
        )?;
        authority.create_annotation(BrowserAnnotation {
            annotation_id: BrowserAnnotationId::new(format!(
                "browser-annotation-{}-{}",
                now.0,
                BROWSER_TAB_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            )),
            browser_session_id: session.browser_session_id.clone(),
            tab_id: tab.tab_id.clone(),
            sequence: 0,
            author: BrowserAnnotationAuthor::User,
            kind,
            anchor,
            comment,
            status: BrowserAnnotationStatus::Active,
            screenshot_artifact_id: Some(screenshot_artifact_id),
            created_at: now,
            updated_at: now,
        })
    }) {
        Ok(annotation) => annotation,
        Err(error) => {
            let _ = delete_browser_annotation_artifact(&state, &artifact_id_for_cleanup);
            return Err(error);
        }
    };
    sync_browser_annotations_to_host(&state, &annotation.tab_id).await?;
    publish_browser_event(
        &state,
        "browser.annotation.created",
        session.owner_workspace_id(),
        require_session_scope(&session)?,
        serde_json::json!({
            "annotation_id": annotation.annotation_id,
            "tab_id": annotation.tab_id,
            "status": annotation.status,
        }),
    );
    Ok((
        StatusCode::CREATED,
        Json(BrowserAnnotationResponse::from(annotation)),
    ))
}

pub(super) async fn persist_browser_annotation_screenshot(
    state: &ApiState,
    session_id: &SessionId,
    tab_id: &BrowserTabId,
    clip: BrowserNormalizedRect,
    host_tab_id: &BrowserTabId,
    navigation_revision: u64,
) -> Result<String, ApiError> {
    let reply = require_browser_host(state)?
        .request(BrowserHostCommand::Screenshot {
            tab_id: host_tab_id.clone(),
            navigation_revision,
            target: None,
            // Chromium clipped capture 会改写 fixed viewport 的滚动位置。
            // 先捕获同一真实可见 viewport，再在内存中只做无损裁剪。
            clip: None,
            full_page: false,
            format: magi_browser_authority::BrowserScreenshotFormat::Png,
            quality: None,
        })
        .await
        .map_err(|error| ApiError::model_invocation_failed("保存浏览器标记截图失败", error))?;
    let metadata = match reply.response.outcome {
        BrowserHostCommandOutcome::Succeeded(result) => match *result {
            BrowserHostCommandResult::BinaryPayload(metadata) => metadata,
            result => {
                return Err(host_outcome_error(
                    "保存浏览器标记截图失败",
                    BrowserHostCommandOutcome::Succeeded(Box::new(result)),
                ));
            }
        },
        outcome => return Err(host_outcome_error("保存浏览器标记截图失败", outcome)),
    };
    let viewport_bytes = reply.binary.ok_or_else(|| {
        ApiError::InternalAssemblyError("浏览器标记截图缺少二进制内容".to_string())
    })?;
    let bytes = crop_browser_screenshot(
        &viewport_bytes,
        magi_browser_authority::BrowserScreenshotFormat::Png,
        clip,
        None,
    )
    .map_err(|error| ApiError::InternalAssemblyError(error.to_string()))?;
    let Some(persistence) = state.runtime_persistence() else {
        return Err(ApiError::Conflict("浏览器标记截图存储不可用".to_string()));
    };
    let Some(state_root) = persistence.state_root() else {
        return Err(ApiError::Conflict(
            "浏览器标记截图存储根目录不可用".to_string(),
        ));
    };
    let filename = format!(
        "annotation-shot-{}-{}-{}.png",
        UtcMillis::now().0,
        tab_id,
        BROWSER_ANNOTATION_ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let relative_id = format!("{}/{}", session_id, filename);
    let path = state_root
        .join(crate::browser_tool_runtime::BROWSER_ARTIFACT_DIR)
        .join(&relative_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| ApiError::internal_assembly("创建浏览器标记截图目录失败", error))?;
    }
    magi_core::fs_atomic::write_atomic(&path, &bytes)
        .map_err(|error| ApiError::internal_assembly("写入浏览器标记截图失败", error))?;
    tracing::debug!(
        session_id = %session_id,
        tab_id = %tab_id,
        mime = %metadata.mime_type,
        artifact_id = %relative_id,
        "已持久化浏览器标记截图 artifact"
    );
    Ok(relative_id)
}

pub(super) fn delete_browser_annotation_artifact(
    state: &ApiState,
    artifact_id: &str,
) -> Result<(), ApiError> {
    let Some(path) = browser_artifact_path(state, artifact_id)? else {
        return Ok(());
    };
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ApiError::internal_assembly(
            "清理失败的浏览器标记截图 artifact 失败",
            error,
        )),
    }
}

pub(super) async fn browser_hit_test(
    state: &ApiState,
    tab_id: &BrowserTabId,
    navigation_revision: u64,
    x: f64,
    y: f64,
) -> Result<BrowserHostHitTest, ApiError> {
    let result = require_host_success(
        require_browser_host(state)?
            .request(BrowserHostCommand::HitTest {
                tab_id: tab_id.clone(),
                navigation_revision,
                normalized_x: x,
                normalized_y: y,
            })
            .await,
        "页面标记命中检测失败",
    )?;
    match result {
        BrowserHostCommandResult::HitTest(hit) => Ok(hit),
        _ => Err(ApiError::InternalAssemblyError(
            "浏览器 Host 命中检测结果无效".to_string(),
        )),
    }
}

pub(super) fn validate_annotation_point(x: f64, y: f64) -> Result<(), ApiError> {
    if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 || x > 1.0 || y > 1.0 {
        return Err(ApiError::InvalidInput(
            "页面标记坐标超出当前浏览器视口".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn validate_annotation_rect(rect: BrowserNormalizedRect) -> Result<(), ApiError> {
    let values = [rect.x, rect.y, rect.width, rect.height];
    if values.iter().any(|value| !value.is_finite())
        || rect.x < 0.0
        || rect.y < 0.0
        || rect.width <= 0.0
        || rect.height <= 0.0
        || rect.x + rect.width > 1.0
        || rect.y + rect.height > 1.0
    {
        return Err(ApiError::InvalidInput(
            "页面区域标记必须位于当前浏览器视口内".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn normalize_hit_bounds(
    viewport_width: u32,
    viewport_height: u32,
    bounds: BrowserHostRect,
) -> Result<BrowserNormalizedRect, ApiError> {
    let width = f64::from(viewport_width);
    let height = f64::from(viewport_height);
    if width <= 0.0
        || height <= 0.0
        || !bounds.x.is_finite()
        || !bounds.y.is_finite()
        || !bounds.width.is_finite()
        || !bounds.height.is_finite()
    {
        return Err(ApiError::InvalidInput("页面元素标记边界无效".to_string()));
    }
    let left = (bounds.x / width).clamp(0.0, 1.0);
    let top = (bounds.y / height).clamp(0.0, 1.0);
    let right = ((bounds.x + bounds.width) / width).clamp(0.0, 1.0);
    let bottom = ((bounds.y + bounds.height) / height).clamp(0.0, 1.0);
    if right <= left || bottom <= top {
        return Err(ApiError::InvalidInput(
            "页面元素当前不在可见视口中".to_string(),
        ));
    }
    Ok(BrowserNormalizedRect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

pub(super) async fn update_annotation_comment(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(annotation_id): Path<String>,
    Json(request): Json<UpdateBrowserAnnotationCommentRequest>,
) -> Result<Json<BrowserAnnotationResponse>, ApiError> {
    require_desktop_browser_capability(&state, &headers, None)?;
    let annotation_id = BrowserAnnotationId::new(annotation_id);
    let comment = request.comment.trim().to_string();
    if comment.is_empty() || comment.chars().count() > 4_000 {
        return Err(ApiError::InvalidInput(
            "标记内容不能为空且不能超过 4000 个字符".to_string(),
        ));
    }
    let annotation = state
        .browser_authority
        .lock()
        .expect("browser authority lock poisoned")
        .annotation(&annotation_id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("浏览器标记不存在", annotation_id.as_str()))?;
    let (_tab, session) = browser_tab_scope(&state, &annotation.tab_id)?;
    ensure_user_control_for_ui(&state, &session, &annotation.tab_id).await?;
    let _control_guard = state
        .browser_control_locks
        .lock_tab(&annotation.tab_id)
        .await;
    let updated = state.mutate_browser_authority(|authority| {
        authority.update_annotation_comment(&annotation_id, comment, UtcMillis::now())
    })?;
    sync_browser_annotations_to_host(&state, &updated.tab_id).await?;
    publish_browser_event(
        &state,
        "browser.annotation.updated",
        session.owner_workspace_id(),
        require_session_scope(&session)?,
        serde_json::json!({
            "annotation_id": updated.annotation_id,
            "tab_id": updated.tab_id,
        }),
    );
    Ok(Json(BrowserAnnotationResponse::from(updated)))
}

pub(super) async fn annotation_artifact(
    State(state): State<ApiState>,
    Path(annotation_id): Path<String>,
    Query(query): Query<AnnotationArtifactQuery>,
) -> Result<Response, ApiError> {
    let annotation_id = BrowserAnnotationId::new(annotation_id);
    let session_id = SessionId::new(query.session_id.trim());
    let artifact_id = {
        let authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock poisoned");
        let annotation = authority
            .annotation(&annotation_id)
            .ok_or_else(|| ApiError::not_found("浏览器标记不存在", annotation_id.as_str()))?;
        let browser_session = authority
            .session(&annotation.browser_session_id)
            .ok_or_else(|| {
                ApiError::not_found("浏览器会话不存在", annotation.browser_session_id.as_str())
            })?;
        if browser_session.magi_session_id() != Some(&session_id) {
            return Err(ApiError::NotFound("浏览器标记不存在".to_string()));
        }
        annotation
            .screenshot_artifact_id
            .clone()
            .ok_or_else(|| ApiError::NotFound("浏览器标记没有截图 artifact".to_string()))?
    };
    let path = browser_artifact_path(&state, &artifact_id)?
        .ok_or_else(|| ApiError::Conflict("浏览器 artifact 存储不可用".to_string()))?;
    let bytes = std::fs::read(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ApiError::NotFound("浏览器标记截图 artifact 不存在".to_string())
        } else {
            ApiError::internal_assembly("读取浏览器标记截图 artifact 失败", error)
        }
    })?;
    Ok((
        StatusCode::OK,
        [
            ("content-type", "image/png"),
            ("cache-control", "private, max-age=60"),
        ],
        bytes,
    )
        .into_response())
}

/// 浏览器工具（如 browser_screenshot）写入 `browser/artifacts/<session>/` 的图片。
/// 只按「会话 + 单段文件名」寻址，供该会话的工具卡片预览；个人会话没有工作区，
/// 不能走工作区文件预览接口。
pub(super) async fn session_browser_artifact(
    State(state): State<ApiState>,
    Path((session_id, file_name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let single_segment = |value: &str| {
        let mut components = std::path::Path::new(value).components();
        matches!(
            (components.next(), components.next()),
            (Some(std::path::Component::Normal(_)), None)
        )
    };
    if !single_segment(&session_id) || !single_segment(&file_name) {
        return Err(ApiError::Conflict("浏览器 artifact 引用无效".to_string()));
    }
    if state
        .session_store
        .session(&SessionId::new(session_id.as_str()))
        .is_none()
    {
        return Err(ApiError::not_found("会话不存在", session_id.as_str()));
    }
    let content_type = match std::path::Path::new(&file_name)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("png") => "image/png",
        Some("jpg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => return Err(ApiError::Conflict("浏览器 artifact 不是图片".to_string())),
    };
    let path = browser_artifact_path(&state, &format!("{session_id}/{file_name}"))?
        .ok_or_else(|| ApiError::Conflict("浏览器 artifact 存储不可用".to_string()))?;
    let bytes = std::fs::read(&path)
        .map_err(|error| ApiError::internal_assembly("读取浏览器 artifact 失败", error))?;
    Ok((
        StatusCode::OK,
        [
            ("content-type", content_type),
            ("cache-control", "private, max-age=3600"),
        ],
        bytes,
    )
        .into_response())
}
