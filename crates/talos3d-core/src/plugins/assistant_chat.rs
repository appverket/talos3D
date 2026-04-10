use std::{env, sync::mpsc, sync::Mutex, thread, time::Duration};

use bevy::prelude::*;
use bevy_egui::egui;
use reqwest::blocking::Client;
use serde_json::{json, Value};

pub const ASSISTANT_PANEL_WIDTH: f32 = 420.0;
const ASSISTANT_TOOL_STEPS: usize = 12;
const ASSISTANT_MAX_TOKENS: u32 = 1600;

const DEFAULT_OPENAI_RESPONSES_URL: &str = "https://api.openai.com/v1/responses";
const DEFAULT_OPENAI_MODEL: &str = "gpt-5.4-mini";
const DEFAULT_ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-4-20250514";

pub struct AssistantChatPlugin;

impl Plugin for AssistantChatPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(AssistantWindowState::default())
            .insert_resource(AssistantChatState::from_environment())
            .insert_resource(PendingAssistantJob::default())
            .add_systems(Update, poll_assistant_jobs);
    }
}

#[derive(Resource, Debug, Clone)]
pub struct AssistantWindowState {
    pub visible: bool,
}

impl Default for AssistantWindowState {
    fn default() -> Self {
        Self { visible: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistantProviderKind {
    ManagedRelay,
    OpenAi,
    Anthropic,
}

impl AssistantProviderKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::ManagedRelay => "Managed Relay",
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
        }
    }
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
    provider: AssistantProviderKind,
    relay_url: String,
    relay_bearer_token: String,
    relay_model: String,
    openai_api_key: String,
    openai_model: String,
    anthropic_api_key: String,
    anthropic_model: String,
    mcp_url: String,
}

#[derive(Resource)]
pub struct AssistantChatState {
    pub provider: AssistantProviderKind,
    pub relay_url: String,
    pub relay_bearer_token: String,
    pub relay_model: String,
    pub openai_api_key: String,
    pub openai_model: String,
    pub anthropic_api_key: String,
    pub anthropic_model: String,
    pub mcp_url: String,
    pub draft: String,
    pub messages: Vec<AssistantChatMessage>,
    pub pending: bool,
    pub last_error: Option<String>,
    pub show_connection: bool,
}

#[derive(Resource, Default)]
pub struct PendingAssistantJob(
    pub Mutex<Option<mpsc::Receiver<Result<Vec<AssistantChatMessage>, String>>>>,
);

impl AssistantChatState {
    pub fn from_environment() -> Self {
        let relay_url = env::var("TALOS3D_ASSISTANT_RELAY_URL").unwrap_or_default();
        let relay_bearer_token = env::var("TALOS3D_ASSISTANT_RELAY_TOKEN").unwrap_or_default();
        let relay_model = env::var("TALOS3D_ASSISTANT_RELAY_MODEL")
            .unwrap_or_else(|_| DEFAULT_OPENAI_MODEL.to_string());
        let openai_api_key = env::var("OPENAI_API_KEY").unwrap_or_default();
        let openai_model = env::var("TALOS3D_ASSISTANT_OPENAI_MODEL")
            .unwrap_or_else(|_| DEFAULT_OPENAI_MODEL.to_string());
        let anthropic_api_key = env::var("ANTHROPIC_API_KEY").unwrap_or_default();
        let anthropic_model = env::var("TALOS3D_ASSISTANT_ANTHROPIC_MODEL")
            .unwrap_or_else(|_| DEFAULT_ANTHROPIC_MODEL.to_string());
        let provider = if !relay_url.is_empty() {
            AssistantProviderKind::ManagedRelay
        } else if !openai_api_key.is_empty() {
            AssistantProviderKind::OpenAi
        } else if !anthropic_api_key.is_empty() {
            AssistantProviderKind::Anthropic
        } else {
            AssistantProviderKind::ManagedRelay
        };

        Self {
            provider,
            relay_url,
            relay_bearer_token,
            relay_model,
            openai_api_key,
            openai_model,
            anthropic_api_key,
            anthropic_model,
            mcp_url: env::var("TALOS3D_ASSISTANT_MCP_URL").unwrap_or_default(),
            draft: String::new(),
            messages: Vec::new(),
            pending: false,
            last_error: None,
            show_connection: false,
        }
    }

    fn runtime_config(&self) -> AssistantRuntimeConfig {
        AssistantRuntimeConfig {
            provider: self.provider,
            relay_url: self.relay_url.trim().to_string(),
            relay_bearer_token: self.relay_bearer_token.trim().to_string(),
            relay_model: self.relay_model.trim().to_string(),
            openai_api_key: self.openai_api_key.trim().to_string(),
            openai_model: self.openai_model.trim().to_string(),
            anthropic_api_key: self.anthropic_api_key.trim().to_string(),
            anthropic_model: self.anthropic_model.trim().to_string(),
            mcp_url: self.mcp_url.trim().to_string(),
        }
    }
}

pub fn draw_assistant_window(
    ctx: &egui::Context,
    state: &mut AssistantChatState,
    window_state: &mut AssistantWindowState,
    pending_job: &PendingAssistantJob,
    default_mcp_url: Option<&str>,
) {
    if !window_state.visible {
        return;
    }
    if state.mcp_url.trim().is_empty() {
        if let Some(url) = default_mcp_url {
            state.mcp_url = url.to_string();
        }
    }

    let available = ctx.available_rect();
    let default_pos = egui::pos2(
        available.max.x - ASSISTANT_PANEL_WIDTH - 8.0,
        available.min.y + 8.0,
    );
    let default_height = (available.height() - 16.0).max(520.0);

    let mut send_now = false;
    egui::Window::new("Assistant")
        .id(egui::Id::new("assistant_chat_window"))
        .open(&mut window_state.visible)
        .default_pos(default_pos)
        .default_width(ASSISTANT_PANEL_WIDTH)
        .default_height(default_height)
        .show(ctx, |ui| {
            // --- Header bar: model badge | gear | overflow menu ---
            ui.horizontal(|ui| {
                let model_label = match state.provider {
                    AssistantProviderKind::ManagedRelay => &state.relay_model,
                    AssistantProviderKind::OpenAi => &state.openai_model,
                    AssistantProviderKind::Anthropic => &state.anthropic_model,
                };
                let badge = egui::RichText::new(model_label.as_str())
                    .small()
                    .color(egui::Color32::from_rgb(180, 200, 220));
                egui::Frame::NONE
                    .fill(egui::Color32::from_rgb(40, 50, 65))
                    .corner_radius(4.0)
                    .inner_margin(egui::Margin::symmetric(6, 2))
                    .show(ui, |ui| {
                        ui.label(badge);
                    });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.menu_button(egui::RichText::new("...").strong(), |ui| {
                        if ui.button("Clear chat").clicked() && !state.pending {
                            state.messages.clear();
                            state.last_error = None;
                            ui.close();
                        }
                    });
                    let gear_label = if state.show_connection { "x" } else { "#" };
                    if ui.button(egui::RichText::new(gear_label).monospace()).clicked() {
                        state.show_connection = !state.show_connection;
                    }
                });
            });

            // --- Settings panel (toggled by gear) ---
            if state.show_connection {
                ui.add_space(4.0);
                egui::Frame::group(ui.style())
                    .fill(egui::Color32::from_rgb(30, 35, 44))
                    .show(ui, |ui| {
                        egui::Grid::new("assistant_settings_grid")
                            .num_columns(2)
                            .spacing([8.0, 4.0])
                            .show(ui, |ui| {
                                ui.label("Provider");
                                egui::ComboBox::from_id_salt("assistant_provider")
                                    .selected_text(state.provider.label())
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut state.provider,
                                            AssistantProviderKind::ManagedRelay,
                                            AssistantProviderKind::ManagedRelay.label(),
                                        );
                                        ui.selectable_value(
                                            &mut state.provider,
                                            AssistantProviderKind::OpenAi,
                                            AssistantProviderKind::OpenAi.label(),
                                        );
                                        ui.selectable_value(
                                            &mut state.provider,
                                            AssistantProviderKind::Anthropic,
                                            AssistantProviderKind::Anthropic.label(),
                                        );
                                    });
                                ui.end_row();

                                ui.label("MCP URL");
                                ui.add(
                                    egui::TextEdit::singleline(&mut state.mcp_url)
                                        .desired_width(ui.available_width()),
                                );
                                ui.end_row();

                                match state.provider {
                                    AssistantProviderKind::ManagedRelay => {
                                        ui.label("Relay URL");
                                        ui.add(egui::TextEdit::singleline(&mut state.relay_url).desired_width(ui.available_width()));
                                        ui.end_row();
                                        ui.label("Bearer");
                                        ui.add(egui::TextEdit::singleline(&mut state.relay_bearer_token).desired_width(ui.available_width()).password(true));
                                        ui.end_row();
                                        ui.label("Model");
                                        ui.add(egui::TextEdit::singleline(&mut state.relay_model).desired_width(ui.available_width()));
                                        ui.end_row();
                                    }
                                    AssistantProviderKind::OpenAi => {
                                        ui.label("API key");
                                        ui.add(egui::TextEdit::singleline(&mut state.openai_api_key).desired_width(ui.available_width()).password(true));
                                        ui.end_row();
                                        ui.label("Model");
                                        ui.add(egui::TextEdit::singleline(&mut state.openai_model).desired_width(ui.available_width()));
                                        ui.end_row();
                                    }
                                    AssistantProviderKind::Anthropic => {
                                        ui.label("API key");
                                        ui.add(egui::TextEdit::singleline(&mut state.anthropic_api_key).desired_width(ui.available_width()).password(true));
                                        ui.end_row();
                                        ui.label("Model");
                                        ui.add(egui::TextEdit::singleline(&mut state.anthropic_model).desired_width(ui.available_width()));
                                        ui.end_row();
                                    }
                                }
                            });
                    });
            }

            ui.separator();

            // --- Chat area ---
            egui::ScrollArea::vertical()
                .id_salt("assistant_chat_messages")
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if state.messages.is_empty() && !state.pending {
                        ui.add_space(40.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                egui::RichText::new("Ask me to create, modify, or explain objects in your scene.")
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
                            ui.label(egui::RichText::new("Thinking...").italics());
                        });
                    }
                    if let Some(error) = &state.last_error {
                        ui.colored_label(egui::Color32::from_rgb(255, 140, 140), error);
                    }
                });

            ui.separator();

            // --- Input bar ---
            ui.horizontal(|ui| {
                let input = ui.add(
                    egui::TextEdit::multiline(&mut state.draft)
                        .desired_width(ui.available_width() - 36.0)
                        .desired_rows(1)
                        .hint_text("Ask about your scene..."),
                );
                let enter_to_send = input.has_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
                if ui
                    .add_enabled(
                        !state.pending && !state.draft.trim().is_empty(),
                        egui::Button::new(egui::RichText::new("\u{2191}").strong())
                            .min_size(egui::vec2(28.0, 28.0)),
                    )
                    .clicked()
                {
                    send_now = true;
                }
                if enter_to_send {
                    send_now = true;
                }
            });
        });

    if send_now {
        // Strip trailing newline that Enter key inserts before we process
        let trimmed = state.draft.trim().to_string();
        state.draft = trimmed;
        if let Err(error) = start_assistant_job(state, pending_job) {
            state.last_error = Some(error);
        }
    }
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
    match receiver.try_recv() {
        Ok(Ok(messages)) => {
            state.messages.extend(messages);
            state.pending = false;
            state.last_error = None;
            *slot = None;
        }
        Ok(Err(error)) => {
            state.messages.push(AssistantChatMessage::error(&error));
            state.pending = false;
            state.last_error = Some(error);
            *slot = None;
        }
        Err(mpsc::TryRecvError::Empty) => {}
        Err(mpsc::TryRecvError::Disconnected) => {
            state.pending = false;
            state.last_error = Some("Assistant worker disconnected".to_string());
            *slot = None;
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

    let config = state.runtime_config();
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
            let result = run_assistant_turn(config, conversation);
            let _ = sender.send(result);
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
    match config.provider {
        AssistantProviderKind::ManagedRelay => {
            if config.relay_url.is_empty() {
                return Err("Managed relay URL is not configured".to_string());
            }
            if config.relay_model.is_empty() {
                return Err("Managed relay model is not configured".to_string());
            }
        }
        AssistantProviderKind::OpenAi => {
            if config.openai_api_key.is_empty() {
                return Err("OpenAI API key is not configured".to_string());
            }
            if config.openai_model.is_empty() {
                return Err("OpenAI model is not configured".to_string());
            }
        }
        AssistantProviderKind::Anthropic => {
            if config.anthropic_api_key.is_empty() {
                return Err("Anthropic API key is not configured".to_string());
            }
            if config.anthropic_model.is_empty() {
                return Err("Anthropic model is not configured".to_string());
            }
        }
    }
    Ok(())
}

fn run_assistant_turn(
    config: AssistantRuntimeConfig,
    conversation: Vec<AssistantChatMessage>,
) -> Result<Vec<AssistantChatMessage>, String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|error| format!("Failed to build assistant HTTP client: {error}"))?;
    let bridge = AssistantMcpBridge::new(client.clone(), config.mcp_url.clone());
    match config.provider {
        AssistantProviderKind::ManagedRelay => run_openai_turn(
            &client,
            &conversation,
            &bridge,
            &config.relay_url,
            &config.relay_bearer_token,
            &config.relay_model,
        ),
        AssistantProviderKind::OpenAi => run_openai_turn(
            &client,
            &conversation,
            &bridge,
            DEFAULT_OPENAI_RESPONSES_URL,
            &config.openai_api_key,
            &config.openai_model,
        ),
        AssistantProviderKind::Anthropic => run_anthropic_turn(
            &client,
            &conversation,
            &bridge,
            &config.anthropic_api_key,
            &config.anthropic_model,
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

fn run_openai_turn(
    client: &Client,
    conversation: &[AssistantChatMessage],
    bridge: &AssistantMcpBridge,
    endpoint: &str,
    bearer_token: &str,
    model: &str,
) -> Result<Vec<AssistantChatMessage>, String> {
    let mut previous_response_id: Option<String> = None;
    let mut next_input = json!(build_openai_input_messages(conversation));
    let mut output_messages = Vec::new();

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

        let response = post_json_with_bearer(client, endpoint, bearer_token, &body)?;
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
                output_messages.push(AssistantChatMessage::tool(format!(
                    "{} {} -> {}",
                    function_call.name,
                    compact_json(&function_call.arguments),
                    compact_json(&result)
                )));
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
            output_messages.push(AssistantChatMessage::assistant(text));
            return Ok(output_messages);
        }
    }

    Err("OpenAI assistant did not produce a final response within the tool step budget".to_string())
}

fn run_anthropic_turn(
    client: &Client,
    conversation: &[AssistantChatMessage],
    bridge: &AssistantMcpBridge,
    api_key: &str,
    model: &str,
) -> Result<Vec<AssistantChatMessage>, String> {
    let mut messages = build_anthropic_messages(conversation);
    let mut output_messages = Vec::new();

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
            DEFAULT_ANTHROPIC_URL,
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
                output_messages.push(AssistantChatMessage::tool(format!(
                    "{} {} -> {}",
                    tool_name,
                    compact_json(&arguments),
                    compact_json(&result)
                )));
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
            output_messages.push(AssistantChatMessage::assistant(text));
            return Ok(output_messages);
        }
    }

    Err(
        "Anthropic assistant did not produce a final response within the tool step budget"
            .to_string(),
    )
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

fn post_json_with_bearer(
    client: &Client,
    endpoint: &str,
    bearer_token: &str,
    body: &Value,
) -> Result<Value, String> {
    if bearer_token.is_empty() {
        return Err("Bearer token is not configured".to_string());
    }
    let response = client
        .post(endpoint)
        .bearer_auth(bearer_token)
        .json(body)
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
