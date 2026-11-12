use super::*;

impl BrowserToolRuntimeDependencies {
    pub fn capabilities(&self) -> BrowserCapabilitySnapshot {
        let host = self
            .host_status
            .read()
            .expect("browser host status lock poisoned")
            .clone();
        BrowserCapabilitySnapshot {
            revision: host.revision,
            in_app_browser_enabled: host.settings.in_app_browser_enabled,
            browser_use_enabled: host.settings.browser_use_enabled,
            devtools_enabled: host.settings.devtools_enabled,
            lan_access_enabled: host.settings.lan_access_enabled,
            host_status: host.status,
            host_protocol_compatible: host.protocol_compatible,
        }
    }

    pub(super) fn publish_browser_event(
        &self,
        event_type: &str,
        session: &magi_browser_authority::BrowserSession,
        payload: Value,
    ) {
        let now = UtcMillis::now();
        let sequence = BROWSER_EVENT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.event_bus.publish(
            EventEnvelope::domain(
                EventId::new(format!(
                    "event-{}-{}-{sequence}",
                    event_type.replace('.', "-"),
                    now.0
                )),
                event_type,
                payload,
            )
            .with_context(EventContext {
                workspace_id: session.owner_workspace_id().cloned(),
                session_id: session.magi_session_id().cloned(),
                ..EventContext::default()
            }),
        );
    }

    pub(super) fn publish_tab_event(
        &self,
        event_type: &str,
        tab: &magi_browser_authority::BrowserTab,
    ) {
        let session = self
            .authority
            .lock()
            .expect("browser authority lock poisoned")
            .session(&tab.browser_session_id)
            .cloned();
        let Some(session) = session else {
            return;
        };
        self.publish_browser_event(
            event_type,
            &session,
            json!({
                "browser_session_id": tab.browser_session_id,
                "tab_id": tab.tab_id,
                "lifecycle": tab.lifecycle,
                "url": tab.url,
                "title": tab.title,
                "navigation_revision": tab.navigation_revision,
            }),
        );
    }
}
