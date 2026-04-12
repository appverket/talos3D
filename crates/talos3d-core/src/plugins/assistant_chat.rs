use std::{env, sync::mpsc, sync::Mutex, thread, time::Duration};

use bevy::prelude::*;
use bevy_egui::egui;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::plugins::document_properties::DocumentProperties;

pub const ASSISTANT_PANEL_DEFAULT_WIDTH: f32 = 320.0;
pub const ASSISTANT_PANEL_MIN_WIDTH: f32 = 280.0;
pub const ASSISTANT_PANEL_MAX_WIDTH: f32 = 520.0;
const ASSISTANT_TOOL_STEPS: usize = 12;
const ASSISTANT_MAX_TOKENS: u32 = 1600;

const DEFAULT_OPENAI_RESPONSES_URL: &str = "https://api.openai.com/v1/responses";
const DEFAULT_OPENAI_CHAT_URL: &str = "https://api.openai.com/v1/chat/completions";
const DEFAULT_OPENAI_MODEL: &str = "gpt-5.4-mini";
const DEFAULT_ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-4-20250514";
const DEFAULT_GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";
const DEFAULT_LM_STUDIO_CHAT_URL: &str = "http://127.0.0.1:1234/v1/chat/completions";
const DEFAULT_OLLAMA_CHAT_URL: &str = "http://127.0.0.1:11434/v1/chat/completions";
const RIGHT_SIDEBAR_STATE_KEY: &str = "right_sidebar_state";
const ASSISTANT_PREFERENCES_KEY: &str = "assistant_chat_preferences";

pub struct AssistantChatPlugin;

impl Plugin for AssistantChatPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(RightSidebarState::default())
            .insert_resource(AssistantChatState::from_environment())
            .insert_resource(PendingAssistantJob::default())
            .add_systems(Update, sync_right_sidebar_state)
            .add_systems(Update, sync_assistant_preferences)
            .add_systems(Update, poll_assistant_jobs);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RightSidebarTab {
    Assistant,
}

impl RightSidebarTab {
    fn label(self) -> &'static str {
        match self {
            Self::Assistant => "Assistant",
        }
    }
}

#[derive(Resource, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RightSidebarState {
    pub visible: bool,
    pub width: f32,
    pub active_tab: RightSidebarTab,
}

impl Default for RightSidebarState {
    fn default() -> Self {
        Self {
            visible: true,
            width: ASSISTANT_PANEL_DEFAULT_WIDTH,
            active_tab: RightSidebarTab::Assistant,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantProviderKind {
    ManagedRelay,
    OpenAi,
    Anthropic,
    Gemini,
    OpenAiCompatible,
}

impl AssistantProviderKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ManagedRelay => "Managed Relay",
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Google Gemini",
            Self::OpenAiCompatible => "Local / OpenAI-Compatible",
        }
    }

    fn supported_protocols(self) -> &'static [AssistantProtocolKind] {
        match self {
            Self::ManagedRelay => &[AssistantProtocolKind::OpenAiResponses],
            Self::OpenAi => &[
                AssistantProtocolKind::OpenAiResponses,
                AssistantProtocolKind::OpenAiChatCompletions,
            ],
            Self::Anthropic => &[AssistantProtocolKind::AnthropicMessages],
            Self::Gemini => &[AssistantProtocolKind::GeminiGenerateContent],
            Self::OpenAiCompatible => &[AssistantProtocolKind::OpenAiChatCompletions],
        }
    }

    fn default_protocol(self) -> AssistantProtocolKind {
        self.supported_protocols()[0]
    }

    fn default_profile_name(self) -> &'static str {
        match self {
            Self::ManagedRelay => "Managed Relay",
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Claude",
            Self::Gemini => "Gemini",
            Self::OpenAiCompatible => "Local Model",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantProtocolKind {
    OpenAiResponses,
    OpenAiChatCompletions,
    AnthropicMessages,
    GeminiGenerateContent,
}

impl AssistantProtocolKind {
    fn label(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "OpenAI Responses",
            Self::OpenAiChatCompletions => "OpenAI Chat Completions",
            Self::AnthropicMessages => "Anthropic Messages",
            Self::GeminiGenerateContent => "Gemini Generate Content",
        }
    }

    fn default_endpoint(self, provider: AssistantProviderKind) -> &'static str {
        match (provider, self) {
            (AssistantProviderKind::ManagedRelay, Self::OpenAiResponses) => "",
            (AssistantProviderKind::OpenAi, Self::OpenAiResponses) => DEFAULT_OPENAI_RESPONSES_URL,
            (AssistantProviderKind::OpenAi, Self::OpenAiChatCompletions) => DEFAULT_OPENAI_CHAT_URL,
            (AssistantProviderKind::Anthropic, Self::AnthropicMessages) => DEFAULT_ANTHROPIC_URL,
            (AssistantProviderKind::Gemini, Self::GeminiGenerateContent) => DEFAULT_GEMINI_API_BASE,
            (AssistantProviderKind::OpenAiCompatible, Self::OpenAiChatCompletions) => {
                DEFAULT_LM_STUDIO_CHAT_URL
            }
            _ => "",
        }
    }

    fn endpoint_label(self) -> &'static str {
        match self {
            Self::GeminiGenerateContent => "API Base",
            _ => "Endpoint",
        }
    }

    fn api_key_label(self, provider: AssistantProviderKind) -> &'static str {
        match provider {
            AssistantProviderKind::ManagedRelay => "Bearer token",
            _ => "API key",
        }
    }

    fn api_key_required(self, provider: AssistantProviderKind) -> bool {
        !matches!(provider, AssistantProviderKind::OpenAiCompatible)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistantPanelTab {
    Chat,
    Settings,
}

impl AssistantPanelTab {
    fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Settings => "Configs",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantConnectionProfile {
    pub id: String,
    pub name: String,
    pub provider: AssistantProviderKind,
    pub protocol: AssistantProtocolKind,
    pub endpoint: String,
    pub api_key: String,
    pub model: String,
}

impl AssistantConnectionProfile {
    fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        provider: AssistantProviderKind,
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let mut profile = Self {
            id: id.into(),
            name: name.into(),
            provider,
            protocol: provider.default_protocol(),
            endpoint: endpoint.into(),
            api_key: api_key.into(),
            model: model.into(),
        };
        profile.normalize();
        profile
    }

    fn normalize(&mut self) {
        if !self.provider.supported_protocols().contains(&self.protocol) {
            self.protocol = self.provider.default_protocol();
        }
        if self.name.trim().is_empty() {
            self.name = self.provider.default_profile_name().to_string();
        }
        if self.endpoint.trim().is_empty() {
            self.endpoint = self.protocol.default_endpoint(self.provider).to_string();
        }
    }

    fn badge_label(&self) -> String {
        let model = if self.model.trim().is_empty() {
            self.protocol.label().to_string()
        } else {
            self.model.trim().to_string()
        };
        format!("{} · {}", self.name.trim(), model)
    }

    fn retarget_provider(&mut self, provider: AssistantProviderKind) {
        let old_default = self.protocol.default_endpoint(self.provider).to_string();
        let old_endpoint = self.endpoint.trim().to_string();
        self.provider = provider;
        if !self.provider.supported_protocols().contains(&self.protocol) {
            self.protocol = self.provider.default_protocol();
        }
        let new_default = self.protocol.default_endpoint(self.provider).to_string();
        if old_endpoint.is_empty() || old_endpoint == old_default {
            self.endpoint = new_default;
        }
        if self.name.trim().is_empty() {
            self.name = self.provider.default_profile_name().to_string();
        }
    }

    fn retarget_protocol(&mut self, protocol: AssistantProtocolKind) {
        if !self.provider.supported_protocols().contains(&protocol) {
            return;
        }
        let old_default = self.protocol.default_endpoint(self.provider).to_string();
        let old_endpoint = self.endpoint.trim().to_string();
        self.protocol = protocol;
        let new_default = self.protocol.default_endpoint(self.provider).to_string();
        if old_endpoint.is_empty() || old_endpoint == old_default {
            self.endpoint = new_default;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssistantPreferences {
    pub active_panel_tab: AssistantPanelTab,
    pub mcp_url: String,
    pub active_profile_id: String,
    pub profiles: Vec<AssistantConnectionProfile>,
}

impl AssistantPreferences {
    fn normalize(&mut self) {
        if self.profiles.is_empty() {
            self.profiles = default_assistant_profiles();
        }
        for profile in &mut self.profiles {
            profile.normalize();
        }
        if self.active_profile_id.trim().is_empty()
            || self
                .profiles
                .iter()
                .all(|profile| profile.id != self.active_profile_id)
        {
            self.active_profile_id = self
                .profiles
                .first()
                .map(|profile| profile.id.clone())
                .unwrap_or_default();
        }
    }

    fn active_profile(&self) -> Option<&AssistantConnectionProfile> {
        self.profiles
            .iter()
            .find(|profile| profile.id == self.active_profile_id)
            .or_else(|| self.profiles.first())
    }

    fn active_profile_mut(&mut self) -> Option<&mut AssistantConnectionProfile> {
        let active_id = self.active_profile_id.clone();
        let index = self
            .profiles
            .iter()
            .position(|profile| profile.id == active_id)
            .unwrap_or(0);
        self.profiles.get_mut(index)
    }

    fn next_profile_id(&self) -> String {
        let mut next = self.profiles.len() + 1;
        loop {
            let candidate = format!("profile-{next}");
            if self.profiles.iter().all(|profile| profile.id != candidate) {
                return candidate;
            }
            next += 1;
        }
    }

    fn add_profile(&mut self, profile: AssistantConnectionProfile) {
        self.active_profile_id = profile.id.clone();
        self.profiles.push(profile);
    }

    fn add_blank_profile(&mut self) {
        self.add_profile(AssistantConnectionProfile::new(
            self.next_profile_id(),
            "New Profile",
            AssistantProviderKind::OpenAi,
            DEFAULT_OPENAI_RESPONSES_URL,
            "",
            DEFAULT_OPENAI_MODEL,
        ));
    }

    fn duplicate_active_profile(&mut self) {
        let Some(mut profile) = self.active_profile().cloned() else {
            return;
        };
        profile.id = self.next_profile_id();
        profile.name = format!("{} Copy", profile.name.trim());
        self.add_profile(profile);
    }

    fn delete_active_profile(&mut self) {
        if self.profiles.len() <= 1 {
            return;
        }
        self.profiles
            .retain(|profile| profile.id != self.active_profile_id);
        self.normalize();
    }
}

fn default_assistant_profiles() -> Vec<AssistantConnectionProfile> {
    vec![
        AssistantConnectionProfile::new(
            "managed-relay",
            "Managed Relay",
            AssistantProviderKind::ManagedRelay,
            env::var("TALOS3D_ASSISTANT_RELAY_URL").unwrap_or_default(),
            env::var("TALOS3D_ASSISTANT_RELAY_TOKEN").unwrap_or_default(),
            env::var("TALOS3D_ASSISTANT_RELAY_MODEL")
                .unwrap_or_else(|_| DEFAULT_OPENAI_MODEL.to_string()),
        ),
        AssistantConnectionProfile::new(
            "openai",
            "OpenAI",
            AssistantProviderKind::OpenAi,
            DEFAULT_OPENAI_RESPONSES_URL,
            env::var("OPENAI_API_KEY").unwrap_or_default(),
            env::var("TALOS3D_ASSISTANT_OPENAI_MODEL")
                .unwrap_or_else(|_| DEFAULT_OPENAI_MODEL.to_string()),
        ),
        AssistantConnectionProfile::new(
            "anthropic",
            "Claude",
            AssistantProviderKind::Anthropic,
            DEFAULT_ANTHROPIC_URL,
            env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
            env::var("TALOS3D_ASSISTANT_ANTHROPIC_MODEL")
                .unwrap_or_else(|_| DEFAULT_ANTHROPIC_MODEL.to_string()),
        ),
        AssistantConnectionProfile::new(
            "gemini",
            "Gemini",
            AssistantProviderKind::Gemini,
            DEFAULT_GEMINI_API_BASE,
            env::var("GEMINI_API_KEY").unwrap_or_default(),
            env::var("TALOS3D_ASSISTANT_GEMINI_MODEL").unwrap_or_default(),
        ),
        AssistantConnectionProfile::new(
            "lm-studio",
            "LM Studio",
            AssistantProviderKind::OpenAiCompatible,
            DEFAULT_LM_STUDIO_CHAT_URL,
            env::var("TALOS3D_ASSISTANT_LMSTUDIO_API_KEY").unwrap_or_default(),
            env::var("TALOS3D_ASSISTANT_LMSTUDIO_MODEL").unwrap_or_default(),
        ),
        AssistantConnectionProfile::new(
            "ollama",
            "Ollama",
            AssistantProviderKind::OpenAiCompatible,
            DEFAULT_OLLAMA_CHAT_URL,
            env::var("TALOS3D_ASSISTANT_OLLAMA_API_KEY").unwrap_or_default(),
            env::var("TALOS3D_ASSISTANT_OLLAMA_MODEL").unwrap_or_default(),
        ),
    ]
}

fn default_assistant_preferences() -> AssistantPreferences {
    let profiles = default_assistant_profiles();
    let active_profile_id = if profiles
        .iter()
        .any(|profile| profile.id == "managed-relay" && !profile.endpoint.trim().is_empty())
    {
        "managed-relay".to_string()
    } else if profiles
        .iter()
        .any(|profile| profile.id == "openai" && !profile.api_key.trim().is_empty())
    {
        "openai".to_string()
    } else if profiles
        .iter()
        .any(|profile| profile.id == "anthropic" && !profile.api_key.trim().is_empty())
    {
        "anthropic".to_string()
    } else if profiles
        .iter()
        .any(|profile| profile.id == "gemini" && !profile.api_key.trim().is_empty())
    {
        "gemini".to_string()
    } else {
        "openai".to_string()
    };

    AssistantPreferences {
        active_panel_tab: AssistantPanelTab::Chat,
        mcp_url: env::var("TALOS3D_ASSISTANT_MCP_URL").unwrap_or_default(),
        active_profile_id,
        profiles,
    }
}

fn serialize_assistant_preferences(preferences: &AssistantPreferences) -> serde_json::Value {
    serde_json::to_value(preferences).unwrap_or_else(|_| json!({}))
}

fn deserialize_assistant_preferences(value: &serde_json::Value) -> Option<AssistantPreferences> {
    let mut preferences: AssistantPreferences = serde_json::from_value(value.clone()).ok()?;
    preferences.normalize();
    Some(preferences)
}

fn sync_assistant_preferences(
    mut state: ResMut<AssistantChatState>,
    mut doc_props: ResMut<DocumentProperties>,
    mut last_serialized: Local<Option<serde_json::Value>>,
) {
    let saved = doc_props
        .domain_defaults
        .get(ASSISTANT_PREFERENCES_KEY)
        .cloned();
    if saved != *last_serialized {
        if let Some(saved_preferences) = saved.as_ref().and_then(deserialize_assistant_preferences)
        {
            if state.preferences != saved_preferences {
                state.preferences = saved_preferences;
            }
        }
        *last_serialized = saved.clone();
    }

    state.preferences.normalize();
    let serialized = serialize_assistant_preferences(&state.preferences);
    if doc_props.domain_defaults.get(ASSISTANT_PREFERENCES_KEY) != Some(&serialized) {
        doc_props
            .domain_defaults
            .insert(ASSISTANT_PREFERENCES_KEY.to_string(), serialized.clone());
    }
    *last_serialized = Some(serialized);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistantChatRole {
    User,
    Assistant,
    Tool,
    Error,
}

#[derive(Debug, Clone)]
pub struct AssistantChatMessage {
    pub role: AssistantChatRole,
    pub content: String,
}

impl AssistantChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: AssistantChatRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: AssistantChatRole::Assistant,
            content: content.into(),
        }
    }

    pub fn tool(content: impl Into<String>) -> Self {
        Self {
            role: AssistantChatRole::Tool,
            content: content.into(),
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            role: AssistantChatRole::Error,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone)]
struct AssistantRuntimeConfig {
    profile: AssistantConnectionProfile,
    mcp_url: String,
}

#[derive(Debug)]
enum AssistantJobEvent {
    Message(AssistantChatMessage),
    Completed,
    Failed(String),
}

#[derive(Resource)]
pub struct AssistantChatState {
    pub preferences: AssistantPreferences,
    pub draft: String,
    pub messages: Vec<AssistantChatMessage>,
    pub pending: bool,
    pub last_error: Option<String>,
}

#[derive(Resource, Default)]
pub struct PendingAssistantJob(Mutex<Option<mpsc::Receiver<AssistantJobEvent>>>);

impl AssistantChatState {
    pub fn from_environment() -> Self {
        Self {
            preferences: default_assistant_preferences(),
            draft: String::new(),
            messages: Vec::new(),
            pending: false,
            last_error: None,
        }
    }

    fn runtime_config(&self) -> Result<AssistantRuntimeConfig, String> {
        let profile = self
            .preferences
            .active_profile()
            .cloned()
            .ok_or_else(|| "Assistant profile list is empty".to_string())?;
        Ok(AssistantRuntimeConfig {
            profile,
            mcp_url: self.preferences.mcp_url.trim().to_string(),
        })
    }
}

pub fn draw_assistant_window(
    ctx: &egui::Context,
    state: &mut AssistantChatState,
    sidebar_state: &mut RightSidebarState,
    pending_job: &PendingAssistantJob,
    default_mcp_url: Option<&str>,
) {
    if !sidebar_state.visible {
        return;
    }
    if state.preferences.mcp_url.trim().is_empty() {
        if let Some(url) = default_mcp_url {
            state.preferences.mcp_url = url.to_string();
        }
    }

    state.preferences.normalize();
    let pixels_per_point = ctx.pixels_per_point().max(f32::EPSILON);
    let min_width_points = ASSISTANT_PANEL_MIN_WIDTH / pixels_per_point;
    let max_width_points = ASSISTANT_PANEL_MAX_WIDTH / pixels_per_point;

    let mut send_now = false;
    let profile_badge = state
        .preferences
        .active_profile()
        .map(AssistantConnectionProfile::badge_label)
        .unwrap_or_else(|| "Assistant".to_string());
    let panel_response = egui::SidePanel::right("assistant_chat_sidebar")
        .resizable(true)
        .default_width(sidebar_state.width / pixels_per_point)
        .width_range(min_width_points..=max_width_points)
        .show(ctx, |ui| {
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                ui.heading(sidebar_state.active_tab.label());
                ui.add_space(6.0);
                ui.selectable_value(
                    &mut state.preferences.active_panel_tab,
                    AssistantPanelTab::Chat,
                    AssistantPanelTab::Chat.label(),
                );
                ui.selectable_value(
                    &mut state.preferences.active_panel_tab,
                    AssistantPanelTab::Settings,
                    AssistantPanelTab::Settings.label(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if assistant_icon_button(
                        ui,
                        AssistantUiIcon::Close,
                        "Hide the assistant sidebar",
                        false,
                    )
                    .clicked()
                    {
                        sidebar_state.visible = false;
                    }
                    if assistant_icon_button(
                        ui,
                        AssistantUiIcon::Settings,
                        "Show assistant settings",
                        state.preferences.active_panel_tab == AssistantPanelTab::Settings,
                    )
                    .clicked()
                    {
                        state.preferences.active_panel_tab =
                            if state.preferences.active_panel_tab == AssistantPanelTab::Settings {
                                AssistantPanelTab::Chat
                            } else {
                                AssistantPanelTab::Settings
                            };
                    }
                    ui.menu_button(egui::RichText::new("...").strong(), |ui| {
                        if ui.button("Clear chat").clicked() && !state.pending {
                            state.messages.clear();
                            state.last_error = None;
                            ui.close();
                        }
                    });
                    let badge = egui::RichText::new(profile_badge.as_str())
                        .small()
                        .color(egui::Color32::from_rgb(180, 200, 220));
                    egui::Frame::NONE
                        .fill(egui::Color32::from_rgb(40, 50, 65))
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::symmetric(6, 2))
                        .show(ui, |ui| {
                            ui.label(badge);
                        });
                });
            });
            ui.separator();

            match sidebar_state.active_tab {
                RightSidebarTab::Assistant => match state.preferences.active_panel_tab {
                    AssistantPanelTab::Chat => {
                        draw_assistant_chat_panel(ui, state, &mut send_now);
                    }
                    AssistantPanelTab::Settings => {
                        draw_assistant_settings_panel(ui, state);
                    }
                },
            }
        });
    sidebar_state.width = (panel_response.response.rect.width() * pixels_per_point)
        .clamp(ASSISTANT_PANEL_MIN_WIDTH, ASSISTANT_PANEL_MAX_WIDTH);

    if send_now {
        let trimmed = state.draft.trim().to_string();
        state.draft = trimmed;
        if let Err(error) = start_assistant_job(state, pending_job) {
            state.last_error = Some(error);
        }
    }
}

fn draw_assistant_chat_panel(
    ui: &mut egui::Ui,
    state: &mut AssistantChatState,
    send_now: &mut bool,
) {
    if let Err(error) = state
        .runtime_config()
        .and_then(|config| validate_assistant_config(&config))
    {
        egui::Frame::group(ui.style())
            .fill(egui::Color32::from_rgb(88, 34, 34))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(error)
                            .color(egui::Color32::from_rgb(255, 220, 220))
                            .small(),
                    );
                    if ui.link("Open Configs").clicked() {
                        state.preferences.active_panel_tab = AssistantPanelTab::Settings;
                    }
                });
            });
        ui.add_space(8.0);
    }

    egui::ScrollArea::vertical()
        .id_salt("assistant_chat_messages")
        .stick_to_bottom(true)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if state.messages.is_empty() && !state.pending {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(
                            "Ask me to inspect, create, modify, or explain objects in your scene.",
                        )
                        .italics()
                        .color(egui::Color32::from_rgb(140, 150, 165)),
                    );
                });
                ui.add_space(40.0);
            }
            for message in &state.messages {
                draw_message_bubble(ui, message);
                ui.add_space(6.0);
            }
            if state.pending {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new("Working...").italics());
                });
            }
            if let Some(error) = &state.last_error {
                ui.colored_label(egui::Color32::from_rgb(255, 140, 140), error);
            }
        });

    ui.separator();
    egui::Frame::group(ui.style())
        .fill(egui::Color32::from_rgb(25, 30, 38))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let input = ui.add(
                    egui::TextEdit::multiline(&mut state.draft)
                        .desired_width((ui.available_width() - 40.0).max(120.0))
                        .desired_rows(2)
                        .hint_text("Ask about your scene..."),
                );
                let enter_to_send = input.has_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
                if assistant_icon_button(
                    ui,
                    AssistantUiIcon::Send,
                    "Send prompt (Enter). Use Shift+Enter for a newline.",
                    false,
                )
                .on_hover_text("Send prompt (Enter). Use Shift+Enter for a newline.")
                .clicked()
                    && !state.pending
                    && !state.draft.trim().is_empty()
                {
                    *send_now = true;
                }
                if enter_to_send && !state.pending && !state.draft.trim().is_empty() {
                    *send_now = true;
                }
            });
        });
}

fn draw_assistant_settings_panel(ui: &mut egui::Ui, state: &mut AssistantChatState) {
    egui::ScrollArea::vertical()
        .id_salt("assistant_settings_panel")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(
                    "Saved configurations persist with the editor state. Use them for Claude, Gemini, OpenAI, and local OpenAI-compatible runtimes such as LM Studio or Ollama.",
                )
                .small()
                .color(egui::Color32::from_rgb(160, 170, 184)),
            );
            ui.add_space(8.0);

            let selected_profile_name = state
                .preferences
                .active_profile()
                .map(|profile| profile.name.clone())
                .unwrap_or_else(|| "No profile".to_string());
            let mut add_profile = false;
            let mut duplicate_profile = false;
            let mut delete_profile = false;
            ui.horizontal(|ui| {
                ui.label("Profile");
                egui::ComboBox::from_id_salt("assistant_profile_select")
                    .selected_text(selected_profile_name)
                    .show_ui(ui, |ui| {
                        for profile in &state.preferences.profiles {
                            ui.selectable_value(
                                &mut state.preferences.active_profile_id,
                                profile.id.clone(),
                                profile.name.as_str(),
                            );
                        }
                    });
                if ui.button("New").clicked() {
                    add_profile = true;
                }
                if ui.button("Duplicate").clicked() {
                    duplicate_profile = true;
                }
                if ui
                    .add_enabled(
                        state.preferences.profiles.len() > 1,
                        egui::Button::new("Delete"),
                    )
                    .clicked()
                {
                    delete_profile = true;
                }
            });
            if add_profile {
                state.preferences.add_blank_profile();
            }
            if duplicate_profile {
                state.preferences.duplicate_active_profile();
            }
            if delete_profile {
                state.preferences.delete_active_profile();
            }

            ui.separator();
            egui::Frame::group(ui.style())
                .fill(egui::Color32::from_rgb(30, 35, 44))
                .show(ui, |ui| {
                    egui::Grid::new("assistant_settings_mcp_grid")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("MCP URL");
                            ui.add(
                                egui::TextEdit::singleline(&mut state.preferences.mcp_url)
                                    .desired_width(ui.available_width()),
                            );
                            ui.end_row();
                        });
                    ui.separator();
                    if let Some(profile) = state.preferences.active_profile_mut() {
                        egui::Grid::new("assistant_settings_grid")
                            .num_columns(2)
                            .spacing([8.0, 6.0])
                            .show(ui, |ui| {
                                ui.label("Name");
                                ui.add(
                                    egui::TextEdit::singleline(&mut profile.name)
                                        .desired_width(ui.available_width()),
                                );
                                ui.end_row();

                                let previous_provider = profile.provider;
                                ui.label("Provider");
                                egui::ComboBox::from_id_salt("assistant_provider")
                                    .selected_text(profile.provider.label())
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut profile.provider,
                                            AssistantProviderKind::ManagedRelay,
                                            AssistantProviderKind::ManagedRelay.label(),
                                        );
                                        ui.selectable_value(
                                            &mut profile.provider,
                                            AssistantProviderKind::OpenAi,
                                            AssistantProviderKind::OpenAi.label(),
                                        );
                                        ui.selectable_value(
                                            &mut profile.provider,
                                            AssistantProviderKind::Anthropic,
                                            AssistantProviderKind::Anthropic.label(),
                                        );
                                        ui.selectable_value(
                                            &mut profile.provider,
                                            AssistantProviderKind::Gemini,
                                            AssistantProviderKind::Gemini.label(),
                                        );
                                        ui.selectable_value(
                                            &mut profile.provider,
                                            AssistantProviderKind::OpenAiCompatible,
                                            AssistantProviderKind::OpenAiCompatible.label(),
                                        );
                                    });
                                if profile.provider != previous_provider {
                                    profile.retarget_provider(profile.provider);
                                }
                                ui.end_row();

                                let previous_protocol = profile.protocol;
                                let supported_protocols = profile.provider.supported_protocols();
                                ui.label("Protocol");
                                egui::ComboBox::from_id_salt("assistant_protocol")
                                    .selected_text(profile.protocol.label())
                                    .show_ui(ui, |ui| {
                                        for protocol in supported_protocols {
                                            ui.selectable_value(
                                                &mut profile.protocol,
                                                *protocol,
                                                protocol.label(),
                                            );
                                        }
                                    });
                                if profile.protocol != previous_protocol {
                                    profile.retarget_protocol(profile.protocol);
                                }
                                ui.end_row();

                                ui.label("Model");
                                ui.add(
                                    egui::TextEdit::singleline(&mut profile.model)
                                        .desired_width(ui.available_width()),
                                );
                                ui.end_row();

                                ui.label(profile.protocol.endpoint_label());
                                ui.add(
                                    egui::TextEdit::singleline(&mut profile.endpoint)
                                        .desired_width(ui.available_width()),
                                );
                                ui.end_row();

                                ui.label(profile.protocol.api_key_label(profile.provider));
                                ui.add(
                                    egui::TextEdit::singleline(&mut profile.api_key)
                                        .desired_width(ui.available_width())
                                        .password(true),
                                );
                                ui.end_row();
                            });
                    }
                });
            if let Some(profile) = state.preferences.active_profile() {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(profile_runtime_hint(profile))
                        .small()
                        .color(egui::Color32::from_rgb(150, 160, 172)),
                );
            }
        });
}

fn profile_runtime_hint(profile: &AssistantConnectionProfile) -> &'static str {
    match profile.provider {
        AssistantProviderKind::ManagedRelay => {
            "Managed Relay uses a bearer-authenticated endpoint and the OpenAI Responses protocol."
        }
        AssistantProviderKind::OpenAi => {
            "OpenAI profiles can use the Responses API or Chat Completions."
        }
        AssistantProviderKind::Anthropic => {
            "Anthropic profiles use the Messages API with tool calling enabled."
        }
        AssistantProviderKind::Gemini => {
            "Gemini profiles expect a Google Generative Language API base URL; the model path is appended automatically."
        }
        AssistantProviderKind::OpenAiCompatible => {
            "Use local OpenAI-compatible endpoints such as LM Studio or Ollama's /v1/chat/completions surface."
        }
    }
}

#[derive(Clone, Copy)]
enum AssistantUiIcon {
    Settings,
    Close,
    Send,
}

fn assistant_icon_button(
    ui: &mut egui::Ui,
    icon: AssistantUiIcon,
    tooltip: &str,
    selected: bool,
) -> egui::Response {
    let size = egui::vec2(28.0, 28.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let visuals = ui.visuals();
    let fill = if selected {
        egui::Color32::from_rgb(50, 78, 108)
    } else if response.hovered() {
        egui::Color32::from_rgb(42, 48, 58)
    } else {
        egui::Color32::from_rgb(24, 28, 34)
    };
    ui.painter().rect(
        rect,
        6.0,
        fill,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(56, 64, 76)),
        egui::StrokeKind::Outside,
    );
    let stroke = egui::Stroke::new(1.7, visuals.strong_text_color());
    match icon {
        AssistantUiIcon::Close => {
            ui.painter().line_segment(
                [
                    rect.left_top() + egui::vec2(8.0, 8.0),
                    rect.right_bottom() - egui::vec2(8.0, 8.0),
                ],
                stroke,
            );
            ui.painter().line_segment(
                [
                    rect.right_top() + egui::vec2(-8.0, 8.0),
                    rect.left_bottom() + egui::vec2(8.0, -8.0),
                ],
                stroke,
            );
        }
        AssistantUiIcon::Send => {
            let p1 = rect.left_center() + egui::vec2(7.0, -4.0);
            let p2 = rect.right_center() + egui::vec2(-6.0, 0.0);
            let p3 = rect.left_center() + egui::vec2(7.0, 4.0);
            let mid = rect.center() + egui::vec2(0.0, 0.0);
            ui.painter().line_segment([p1, p2], stroke);
            ui.painter().line_segment([p3, p2], stroke);
            ui.painter().line_segment([p1, mid], stroke);
            ui.painter().line_segment([p3, mid], stroke);
        }
        AssistantUiIcon::Settings => {
            let center = rect.center();
            let radius = 5.0;
            for index in 0..8 {
                let angle = index as f32 * std::f32::consts::TAU / 8.0;
                let dir = egui::vec2(angle.cos(), angle.sin());
                ui.painter().line_segment(
                    [center + dir * (radius + 1.0), center + dir * (radius + 4.0)],
                    stroke,
                );
            }
            ui.painter().circle_stroke(center, radius, stroke);
            ui.painter().circle_stroke(center, 2.0, stroke);
        }
    }
    response.on_hover_text(tooltip)
}

fn sync_right_sidebar_state(
    mut sidebar_state: ResMut<RightSidebarState>,
    mut doc_props: ResMut<DocumentProperties>,
    mut last_serialized: Local<Option<serde_json::Value>>,
) {
    let saved = doc_props
        .domain_defaults
        .get(RIGHT_SIDEBAR_STATE_KEY)
        .cloned();
    if saved != *last_serialized {
        if let Some(saved_state) = saved.as_ref().and_then(deserialize_right_sidebar_state) {
            if *sidebar_state != saved_state {
                *sidebar_state = saved_state;
            }
        } else if saved.is_none() && last_serialized.is_some() {
            *sidebar_state = RightSidebarState::default();
        }
        *last_serialized = saved.clone();
    }

    let serialized = serialize_right_sidebar_state(&sidebar_state);
    if doc_props.domain_defaults.get(RIGHT_SIDEBAR_STATE_KEY) != Some(&serialized) {
        doc_props
            .domain_defaults
            .insert(RIGHT_SIDEBAR_STATE_KEY.to_string(), serialized.clone());
    }
    *last_serialized = Some(serialized);
}

fn serialize_right_sidebar_state(state: &RightSidebarState) -> serde_json::Value {
    serde_json::to_value(state).unwrap_or_else(|_| json!({}))
}

fn deserialize_right_sidebar_state(value: &serde_json::Value) -> Option<RightSidebarState> {
    let mut state: RightSidebarState = serde_json::from_value(value.clone()).ok()?;
    state.width = state
        .width
        .clamp(ASSISTANT_PANEL_MIN_WIDTH, ASSISTANT_PANEL_MAX_WIDTH);
    Some(state)
}

fn draw_message_bubble(ui: &mut egui::Ui, message: &AssistantChatMessage) {
    let (label, fill, text_color, monospace) = match message.role {
        AssistantChatRole::User => (
            "You",
            egui::Color32::from_rgb(38, 61, 86),
            egui::Color32::from_rgb(240, 245, 250),
            false,
        ),
        AssistantChatRole::Assistant => (
            "Assistant",
            egui::Color32::from_rgb(32, 38, 48),
            egui::Color32::from_rgb(235, 240, 248),
            false,
        ),
        AssistantChatRole::Tool => (
            "MCP",
            egui::Color32::from_rgb(28, 48, 42),
            egui::Color32::from_rgb(213, 235, 226),
            true,
        ),
        AssistantChatRole::Error => (
            "Error",
            egui::Color32::from_rgb(88, 34, 34),
            egui::Color32::from_rgb(255, 220, 220),
            false,
        ),
    };

    egui::Frame::group(ui.style()).fill(fill).show(ui, |ui| {
        ui.label(egui::RichText::new(label).strong().color(text_color));
        let text = if monospace {
            egui::RichText::new(&message.content)
                .monospace()
                .small()
                .color(text_color)
        } else {
            egui::RichText::new(&message.content).color(text_color)
        };
        ui.label(text);
    });
}

fn poll_assistant_jobs(
    mut state: ResMut<AssistantChatState>,
    pending_job: Res<PendingAssistantJob>,
) {
    let Ok(mut slot) = pending_job.0.lock() else {
        state.pending = false;
        state.last_error = Some("Assistant job lock is poisoned".to_string());
        return;
    };
    let Some(receiver) = slot.as_ref() else {
        return;
    };
    loop {
        match receiver.try_recv() {
            Ok(AssistantJobEvent::Message(message)) => {
                state.messages.push(message);
                state.last_error = None;
            }
            Ok(AssistantJobEvent::Completed) => {
                state.pending = false;
                state.last_error = None;
                *slot = None;
                break;
            }
            Ok(AssistantJobEvent::Failed(error)) => {
                state.messages.push(AssistantChatMessage::error(&error));
                state.pending = false;
                state.last_error = Some(error);
                *slot = None;
                break;
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                state.pending = false;
                state.last_error = Some("Assistant worker disconnected".to_string());
                *slot = None;
                break;
            }
        }
    }
}

fn start_assistant_job(
    state: &mut AssistantChatState,
    pending_job: &PendingAssistantJob,
) -> Result<(), String> {
    let prompt = state.draft.trim().to_string();
    if prompt.is_empty() {
        return Err("Assistant prompt is empty".to_string());
    }
    if state.pending {
        return Err("Assistant is already processing a request".to_string());
    }

    let config = state.runtime_config()?;
    validate_assistant_config(&config)?;

    state.messages.push(AssistantChatMessage::user(prompt));
    state.draft.clear();
    state.pending = true;
    state.last_error = None;

    let conversation = state.messages.clone();
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("talos3d-assistant".to_string())
        .spawn(move || {
            let result = run_assistant_turn(config, conversation, &sender);
            let event = match result {
                Ok(()) => AssistantJobEvent::Completed,
                Err(error) => AssistantJobEvent::Failed(error),
            };
            let _ = sender.send(event);
        })
        .map_err(|error| format!("Failed to start assistant thread: {error}"))?;

    let mut slot = pending_job
        .0
        .lock()
        .map_err(|_| "Assistant job lock is poisoned".to_string())?;
    *slot = Some(receiver);
    Ok(())
}

fn validate_assistant_config(config: &AssistantRuntimeConfig) -> Result<(), String> {
    if config.mcp_url.trim().is_empty() {
        return Err("Assistant MCP URL is not configured".to_string());
    }
    let profile = &config.profile;
    if profile.model.trim().is_empty() {
        return Err(format!("{} model is not configured", profile.name.trim()));
    }
    if profile.endpoint.trim().is_empty() {
        return Err(format!(
            "{} endpoint is not configured",
            profile.protocol.endpoint_label()
        ));
    }
    if profile.protocol.api_key_required(profile.provider) && profile.api_key.trim().is_empty() {
        return Err(format!(
            "{} is not configured",
            profile.protocol.api_key_label(profile.provider)
        ));
    }
    Ok(())
}

fn run_assistant_turn(
    config: AssistantRuntimeConfig,
    conversation: Vec<AssistantChatMessage>,
    sender: &mpsc::Sender<AssistantJobEvent>,
) -> Result<(), String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|error| format!("Failed to build assistant HTTP client: {error}"))?;
    let bridge = AssistantMcpBridge::new(client.clone(), config.mcp_url.clone());
    match config.profile.protocol {
        AssistantProtocolKind::OpenAiResponses => run_openai_turn(
            &client,
            &conversation,
            &bridge,
            sender,
            &config.profile.endpoint,
            &config.profile.api_key,
            &config.profile.model,
        ),
        AssistantProtocolKind::OpenAiChatCompletions => run_openai_chat_completions_turn(
            &client,
            &conversation,
            &bridge,
            sender,
            &config.profile.endpoint,
            &config.profile.api_key,
            &config.profile.model,
        ),
        AssistantProtocolKind::AnthropicMessages => run_anthropic_turn(
            &client,
            &conversation,
            &bridge,
            sender,
            &config.profile.endpoint,
            &config.profile.api_key,
            &config.profile.model,
        ),
        AssistantProtocolKind::GeminiGenerateContent => run_gemini_turn(
            &client,
            &conversation,
            &bridge,
            sender,
            &config.profile.endpoint,
            &config.profile.api_key,
            &config.profile.model,
        ),
    }
}

#[derive(Debug, Clone)]
struct AssistantToolDef {
    name: &'static str,
    description: &'static str,
    parameters: Value,
}

fn assistant_tool_defs() -> [AssistantToolDef; 2] {
    [
        AssistantToolDef {
            name: "mcp_list_tools",
            description: "List Talos3D MCP tools and their input schemas. Call this first when you need to discover exact tool names or argument shapes.",
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
        AssistantToolDef {
            name: "mcp_call_tool",
            description: "Call a Talos3D MCP tool by name with a JSON object of arguments. Use this for all model inspection and editing work.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "tool_name": { "type": "string" },
                    "arguments": { "type": "object" }
                },
                "required": ["tool_name", "arguments"],
                "additionalProperties": false
            }),
        },
    ]
}

fn assistant_system_prompt(mcp_url: &str) -> String {
    format!(
        "You are the Talos3D in-app assistant. Use Talos3D MCP tools for any model inspection, creation, deletion, transform, material, lighting, definition, layer, view, import, screenshot, or persistence action. Never claim an edit succeeded unless a tool result confirms it. Be concise and operational. If you do not know the exact MCP tool name or argument shape, call mcp_list_tools before editing. The configured MCP endpoint is {mcp_url}."
    )
}

fn emit_assistant_message(sender: &mpsc::Sender<AssistantJobEvent>, message: AssistantChatMessage) {
    let _ = sender.send(AssistantJobEvent::Message(message));
}

fn run_openai_turn(
    client: &Client,
    conversation: &[AssistantChatMessage],
    bridge: &AssistantMcpBridge,
    sender: &mpsc::Sender<AssistantJobEvent>,
    endpoint: &str,
    bearer_token: &str,
    model: &str,
) -> Result<(), String> {
    let mut previous_response_id: Option<String> = None;
    let mut next_input = json!(build_openai_input_messages(conversation));

    for _ in 0..ASSISTANT_TOOL_STEPS {
        let body = if let Some(previous_response_id) = previous_response_id.as_ref() {
            json!({
                "model": model,
                "previous_response_id": previous_response_id,
                "input": next_input,
                "tools": build_openai_tools(),
            })
        } else {
            json!({
                "model": model,
                "instructions": assistant_system_prompt(&bridge.base_url),
                "input": next_input,
                "tools": build_openai_tools(),
            })
        };

        let response = post_json_with_optional_bearer(
            client,
            endpoint,
            if bearer_token.trim().is_empty() {
                None
            } else {
                Some(bearer_token)
            },
            &body,
        )?;
        previous_response_id = response
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let function_calls = extract_openai_function_calls(&response)?;
        if !function_calls.is_empty() {
            let mut tool_outputs = Vec::new();
            for function_call in function_calls {
                let result =
                    execute_assistant_tool(bridge, &function_call.name, &function_call.arguments)?;
                emit_assistant_message(
                    sender,
                    AssistantChatMessage::tool(format!(
                        "{} {} -> {}",
                        function_call.name,
                        compact_json(&function_call.arguments),
                        compact_json(&result)
                    )),
                );
                tool_outputs.push(json!({
                    "type": "function_call_output",
                    "call_id": function_call.call_id,
                    "output": serde_json::to_string(&result).unwrap_or_else(|_| "null".to_string()),
                }));
            }
            next_input = Value::Array(tool_outputs);
            continue;
        }

        let text = extract_openai_text(&response);
        if !text.trim().is_empty() {
            emit_assistant_message(sender, AssistantChatMessage::assistant(text));
            return Ok(());
        }
    }

    Err("OpenAI assistant did not produce a final response within the tool step budget".to_string())
}

fn run_openai_chat_completions_turn(
    client: &Client,
    conversation: &[AssistantChatMessage],
    bridge: &AssistantMcpBridge,
    sender: &mpsc::Sender<AssistantJobEvent>,
    endpoint: &str,
    api_key: &str,
    model: &str,
) -> Result<(), String> {
    let mut messages =
        build_openai_chat_messages(conversation, &assistant_system_prompt(&bridge.base_url));

    for _ in 0..ASSISTANT_TOOL_STEPS {
        let body = json!({
            "model": model,
            "messages": messages,
            "tools": build_openai_chat_tools(),
            "tool_choice": "auto",
        });
        let response = post_json_with_optional_bearer(
            client,
            endpoint,
            if api_key.trim().is_empty() {
                None
            } else {
                Some(api_key)
            },
            &body,
        )?;
        let message = response
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .cloned()
            .ok_or_else(|| {
                "OpenAI chat completion response did not contain a message".to_string()
            })?;

        let tool_calls = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !tool_calls.is_empty() {
            messages.push(json!({
                "role": "assistant",
                "content": message.get("content").cloned().unwrap_or_else(|| json!("")),
                "tool_calls": Value::Array(tool_calls.clone()),
            }));
            for tool_call in tool_calls {
                let function = tool_call
                    .get("function")
                    .ok_or_else(|| "OpenAI chat tool call missing function".to_string())?;
                let tool_name = function
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "OpenAI chat tool call missing name".to_string())?;
                let call_id = tool_call
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(tool_name);
                let arguments = function
                    .get("arguments")
                    .and_then(Value::as_str)
                    .map(|text| serde_json::from_str(text).unwrap_or_else(|_| json!({})))
                    .unwrap_or_else(|| json!({}));
                let result = execute_assistant_tool(bridge, tool_name, &arguments)?;
                emit_assistant_message(
                    sender,
                    AssistantChatMessage::tool(format!(
                        "{} {} -> {}",
                        tool_name,
                        compact_json(&arguments),
                        compact_json(&result)
                    )),
                );
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": serde_json::to_string(&result).unwrap_or_else(|_| "null".to_string()),
                }));
            }
            continue;
        }

        let text = message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if !text.is_empty() {
            emit_assistant_message(sender, AssistantChatMessage::assistant(text));
            return Ok(());
        }
    }

    Err(
        "OpenAI chat assistant did not produce a final response within the tool step budget"
            .to_string(),
    )
}

fn run_anthropic_turn(
    client: &Client,
    conversation: &[AssistantChatMessage],
    bridge: &AssistantMcpBridge,
    sender: &mpsc::Sender<AssistantJobEvent>,
    endpoint: &str,
    api_key: &str,
    model: &str,
) -> Result<(), String> {
    let mut messages = build_anthropic_messages(conversation);

    for _ in 0..ASSISTANT_TOOL_STEPS {
        let body = json!({
            "model": model,
            "max_tokens": ASSISTANT_MAX_TOKENS,
            "system": assistant_system_prompt(&bridge.base_url),
            "messages": messages,
            "tools": build_anthropic_tools(),
        });
        let response = post_json_with_headers(
            client,
            endpoint,
            &[("x-api-key", api_key), ("anthropic-version", "2023-06-01")],
            &body,
        )?;
        let content = response
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let tool_uses = content
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
            .cloned()
            .collect::<Vec<_>>();

        if !tool_uses.is_empty() {
            messages.push(json!({
                "role": "assistant",
                "content": Value::Array(content.clone()),
            }));
            let mut tool_results = Vec::new();
            for tool_use in tool_uses {
                let tool_name = tool_use
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Anthropic tool_use missing name".to_string())?;
                let tool_use_id = tool_use
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Anthropic tool_use missing id".to_string())?;
                let arguments = tool_use.get("input").cloned().unwrap_or_else(|| json!({}));
                let result = execute_assistant_tool(bridge, tool_name, &arguments)?;
                emit_assistant_message(
                    sender,
                    AssistantChatMessage::tool(format!(
                        "{} {} -> {}",
                        tool_name,
                        compact_json(&arguments),
                        compact_json(&result)
                    )),
                );
                tool_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": serde_json::to_string(&result).unwrap_or_else(|_| "null".to_string()),
                }));
            }
            messages.push(json!({
                "role": "user",
                "content": tool_results,
            }));
            continue;
        }

        let text = content
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            emit_assistant_message(sender, AssistantChatMessage::assistant(text));
            return Ok(());
        }
    }

    Err(
        "Anthropic assistant did not produce a final response within the tool step budget"
            .to_string(),
    )
}

fn run_gemini_turn(
    client: &Client,
    conversation: &[AssistantChatMessage],
    bridge: &AssistantMcpBridge,
    sender: &mpsc::Sender<AssistantJobEvent>,
    api_base: &str,
    api_key: &str,
    model: &str,
) -> Result<(), String> {
    let mut contents = build_gemini_contents(conversation);
    let endpoint = gemini_generate_content_endpoint(api_base, model);

    for _ in 0..ASSISTANT_TOOL_STEPS {
        let body = json!({
            "system_instruction": {
                "parts": [{ "text": assistant_system_prompt(&bridge.base_url) }]
            },
            "contents": contents,
            "tools": [{
                "functionDeclarations": build_gemini_function_declarations()
            }],
        });
        let response =
            post_json_with_query_and_headers(client, &endpoint, &[("key", api_key)], &[], &body)?;
        let content = response
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
            .and_then(|candidate| candidate.get("content"))
            .cloned()
            .ok_or_else(|| "Gemini response did not include candidate content".to_string())?;
        let parts = content
            .get("parts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let function_calls = parts
            .iter()
            .filter_map(|part| part.get("functionCall").cloned())
            .collect::<Vec<_>>();
        if !function_calls.is_empty() {
            contents.push(content.clone());
            let mut responses = Vec::new();
            for function_call in function_calls {
                let tool_name = function_call
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "Gemini functionCall missing name".to_string())?;
                let arguments = function_call
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let result = execute_assistant_tool(bridge, tool_name, &arguments)?;
                emit_assistant_message(
                    sender,
                    AssistantChatMessage::tool(format!(
                        "{} {} -> {}",
                        tool_name,
                        compact_json(&arguments),
                        compact_json(&result)
                    )),
                );
                responses.push(json!({
                    "functionResponse": {
                        "name": tool_name,
                        "response": {
                            "result": result
                        }
                    }
                }));
            }
            contents.push(json!({
                "role": "user",
                "parts": responses,
            }));
            continue;
        }

        let text = parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if !text.trim().is_empty() {
            emit_assistant_message(sender, AssistantChatMessage::assistant(text));
            return Ok(());
        }
    }

    Err("Gemini assistant did not produce a final response within the tool step budget".to_string())
}

#[derive(Debug)]
struct OpenAiFunctionCall {
    name: String,
    call_id: String,
    arguments: Value,
}

fn extract_openai_function_calls(response: &Value) -> Result<Vec<OpenAiFunctionCall>, String> {
    let Some(output) = response.get("output").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut function_calls = Vec::new();
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            continue;
        }
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "OpenAI function_call missing name".to_string())?
            .to_string();
        let call_id = item
            .get("call_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "OpenAI function_call missing call_id".to_string())?
            .to_string();
        let arguments = item
            .get("arguments")
            .and_then(Value::as_str)
            .map(|arguments| serde_json::from_str(arguments).unwrap_or_else(|_| json!({})))
            .unwrap_or_else(|| json!({}));
        function_calls.push(OpenAiFunctionCall {
            name,
            call_id,
            arguments,
        });
    }
    Ok(function_calls)
}

fn extract_openai_text(response: &Value) -> String {
    if let Some(text) = response.get("output_text").and_then(Value::as_str) {
        if !text.trim().is_empty() {
            return text.to_string();
        }
    }
    response
        .get("output")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
                .flat_map(|item| {
                    item.get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                })
                .filter_map(|content| {
                    content
                        .get("text")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .or_else(|| {
                            content
                                .get("text")
                                .and_then(|text| text.get("value"))
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned)
                        })
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn build_openai_input_messages(conversation: &[AssistantChatMessage]) -> Vec<Value> {
    conversation
        .iter()
        .filter_map(|message| match message.role {
            AssistantChatRole::User => Some(json!({
                "role": "user",
                "content": message.content,
            })),
            AssistantChatRole::Assistant => Some(json!({
                "role": "assistant",
                "content": message.content,
            })),
            AssistantChatRole::Tool | AssistantChatRole::Error => None,
        })
        .collect()
}

fn build_openai_chat_messages(
    conversation: &[AssistantChatMessage],
    system_prompt: &str,
) -> Vec<Value> {
    let mut messages = vec![json!({
        "role": "system",
        "content": system_prompt,
    })];
    messages.extend(
        conversation
            .iter()
            .filter_map(|message| match message.role {
                AssistantChatRole::User => Some(json!({
                    "role": "user",
                    "content": message.content,
                })),
                AssistantChatRole::Assistant => Some(json!({
                    "role": "assistant",
                    "content": message.content,
                })),
                AssistantChatRole::Tool | AssistantChatRole::Error => None,
            }),
    );
    messages
}

fn build_openai_tools() -> Vec<Value> {
    assistant_tool_defs()
        .into_iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            })
        })
        .collect()
}

fn build_openai_chat_tools() -> Vec<Value> {
    assistant_tool_defs()
        .into_iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                }
            })
        })
        .collect()
}

fn build_anthropic_messages(conversation: &[AssistantChatMessage]) -> Vec<Value> {
    conversation
        .iter()
        .filter_map(|message| match message.role {
            AssistantChatRole::User => Some(json!({
                "role": "user",
                "content": message.content,
            })),
            AssistantChatRole::Assistant => Some(json!({
                "role": "assistant",
                "content": message.content,
            })),
            AssistantChatRole::Tool | AssistantChatRole::Error => None,
        })
        .collect()
}

fn build_gemini_contents(conversation: &[AssistantChatMessage]) -> Vec<Value> {
    conversation
        .iter()
        .filter_map(|message| match message.role {
            AssistantChatRole::User => Some(json!({
                "role": "user",
                "parts": [{ "text": message.content }],
            })),
            AssistantChatRole::Assistant => Some(json!({
                "role": "model",
                "parts": [{ "text": message.content }],
            })),
            AssistantChatRole::Tool | AssistantChatRole::Error => None,
        })
        .collect()
}

fn build_anthropic_tools() -> Vec<Value> {
    assistant_tool_defs()
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.parameters,
            })
        })
        .collect()
}

fn build_gemini_function_declarations() -> Vec<Value> {
    assistant_tool_defs()
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            })
        })
        .collect()
}

fn gemini_generate_content_endpoint(api_base: &str, model: &str) -> String {
    let base = api_base.trim_end_matches('/');
    if base.ends_with(":generateContent") {
        return base.to_string();
    }
    format!("{base}/models/{model}:generateContent")
}

fn execute_assistant_tool(
    bridge: &AssistantMcpBridge,
    tool_name: &str,
    arguments: &Value,
) -> Result<Value, String> {
    match tool_name {
        "mcp_list_tools" => bridge.list_tools(),
        "mcp_call_tool" => {
            let target_tool = arguments
                .get("tool_name")
                .and_then(Value::as_str)
                .ok_or_else(|| "mcp_call_tool requires tool_name".to_string())?;
            let target_arguments = arguments
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            bridge.call_tool(target_tool, target_arguments)
        }
        other => Err(format!("Unknown assistant tool: {other}")),
    }
}

#[derive(Debug, Clone)]
struct AssistantMcpBridge {
    client: Client,
    base_url: String,
}

impl AssistantMcpBridge {
    fn new(client: Client, base_url: String) -> Self {
        Self { client, base_url }
    }

    fn rpc(&self, method: &str, params: Value) -> Result<Value, String> {
        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        post_json_with_headers(
            &self.client,
            &self.base_url,
            &[("accept", "application/json, text/event-stream")],
            &payload,
        )
    }

    fn list_tools(&self) -> Result<Value, String> {
        let response = self.rpc("tools/list", json!({}))?;
        Ok(response
            .get("result")
            .and_then(|result| result.get("tools"))
            .cloned()
            .unwrap_or_else(|| json!([])))
    }

    fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<Value, String> {
        let response = self.rpc(
            "tools/call",
            json!({
                "name": tool_name,
                "arguments": arguments,
            }),
        )?;
        let result = response
            .get("result")
            .ok_or_else(|| "MCP tool call returned no result".to_string())?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            return Err(format!(
                "MCP tool {} returned an error: {}",
                tool_name,
                compact_json(result)
            ));
        }
        let Some(first_content) = result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|content| content.first())
        else {
            return Ok(json!(null));
        };
        let Some(text) = first_content.get("text").and_then(Value::as_str) else {
            return Ok(first_content.clone());
        };
        Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string())))
    }
}

fn post_json_with_optional_bearer(
    client: &Client,
    endpoint: &str,
    bearer_token: Option<&str>,
    body: &Value,
) -> Result<Value, String> {
    let mut request = client.post(endpoint).json(body);
    if let Some(token) = bearer_token.filter(|token| !token.trim().is_empty()) {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .map_err(|error| format!("HTTP request to {endpoint} failed: {error}"))?;
    decode_json_response(response)
}

fn post_json_with_headers(
    client: &Client,
    endpoint: &str,
    headers: &[(&str, &str)],
    body: &Value,
) -> Result<Value, String> {
    let mut request = client.post(endpoint).json(body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request
        .send()
        .map_err(|error| format!("HTTP request to {endpoint} failed: {error}"))?;
    decode_json_response(response)
}

fn post_json_with_query_and_headers(
    client: &Client,
    endpoint: &str,
    query: &[(&str, &str)],
    headers: &[(&str, &str)],
    body: &Value,
) -> Result<Value, String> {
    let mut request = client.post(endpoint).query(query).json(body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request
        .send()
        .map_err(|error| format!("HTTP request to {endpoint} failed: {error}"))?;
    decode_json_response(response)
}

fn decode_json_response(response: reqwest::blocking::Response) -> Result<Value, String> {
    let status = response.status();
    let text = response
        .text()
        .map_err(|error| format!("Failed to read HTTP response body: {error}"))?;
    if !status.is_success() {
        return Err(format!("HTTP {}: {}", status, text));
    }
    serde_json::from_str(&text)
        .map_err(|error| format!("Failed to parse JSON response: {error}; body: {text}"))
}

fn compact_json(value: &Value) -> String {
    let mut text = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
    if text.len() > 220 {
        text.truncate(217);
        text.push_str("...");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn right_sidebar_state_round_trips_through_json() {
        let state = RightSidebarState {
            visible: false,
            width: 412.0,
            active_tab: RightSidebarTab::Assistant,
        };

        let decoded = deserialize_right_sidebar_state(&serialize_right_sidebar_state(&state))
            .expect("sidebar state should deserialize");
        assert_eq!(decoded, state);
    }

    #[test]
    fn right_sidebar_state_clamps_persisted_width() {
        let decoded = deserialize_right_sidebar_state(&json!({
            "visible": true,
            "width": 9999.0,
            "active_tab": "assistant"
        }))
        .expect("sidebar state should deserialize");

        assert_eq!(decoded.width, ASSISTANT_PANEL_MAX_WIDTH);
    }

    #[test]
    fn assistant_preferences_round_trip_and_normalize() {
        let decoded = deserialize_assistant_preferences(&json!({
            "active_panel_tab": "settings",
            "mcp_url": "http://127.0.0.1:24865/mcp",
            "active_profile_id": "missing",
            "profiles": [
                {
                    "id": "local",
                    "name": "LM Studio",
                    "provider": "open_ai_compatible",
                    "protocol": "anthropic_messages",
                    "endpoint": "",
                    "api_key": "",
                    "model": "local-model"
                }
            ]
        }))
        .expect("assistant preferences should deserialize");

        assert_eq!(decoded.active_panel_tab, AssistantPanelTab::Settings);
        assert_eq!(decoded.active_profile_id, "local");
        assert_eq!(
            decoded.profiles[0].protocol,
            AssistantProtocolKind::OpenAiChatCompletions
        );
        assert_eq!(decoded.profiles[0].endpoint, DEFAULT_LM_STUDIO_CHAT_URL);
    }

    #[test]
    fn gemini_endpoint_appends_model_path_once() {
        assert_eq!(
            gemini_generate_content_endpoint(DEFAULT_GEMINI_API_BASE, "gemini-test"),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-test:generateContent"
        );
        assert_eq!(
            gemini_generate_content_endpoint(
                "https://generativelanguage.googleapis.com/v1beta/models/gemini-test:generateContent",
                "ignored"
            ),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-test:generateContent"
        );
    }

    #[test]
    fn extract_openai_function_calls_parses_arguments() {
        let response = json!({
            "output": [
                {
                    "type": "function_call",
                    "name": "mcp_call_tool",
                    "call_id": "call_123",
                    "arguments": "{\"tool_name\":\"list_entities\",\"arguments\":{}}"
                }
            ]
        });

        let calls = extract_openai_function_calls(&response).expect("function calls parse");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "mcp_call_tool");
        assert_eq!(calls[0].call_id, "call_123");
        assert_eq!(calls[0].arguments["tool_name"], json!("list_entities"));
    }

    #[test]
    fn extract_openai_text_reads_message_content() {
        let response = json!({
            "output": [
                {
                    "type": "message",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "Created the light and updated the scene."
                        }
                    ]
                }
            ]
        });

        assert_eq!(
            extract_openai_text(&response),
            "Created the light and updated the scene."
        );
    }
}
