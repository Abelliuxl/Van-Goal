use super::*;

impl XueXiaoMiBackend {
    pub fn restore_model_preference(&mut self, preference: Option<ModelPreference>) {
        self.model_preference = preference;
    }

    pub(super) async fn load_models(&mut self, config: &BackendConfig) -> Result<()> {
        let catalog = self.get(config, "api/models").await?;
        let models: Vec<GatewayModel> = serde_json::from_value(
            catalog
                .get("models")
                .cloned()
                .ok_or_else(|| anyhow!("The gateway did not return its models."))?,
        )?;
        if models.is_empty()
            || models
                .iter()
                .any(|m| m.id.is_empty() || m.thinking_levels.is_empty())
        {
            return Err(anyhow!(
                "The gateway returned an empty model or thinking catalog."
            ));
        }
        let defaults = catalog.get("defaults").cloned().unwrap_or_default();
        let saved = self
            .model_preference
            .as_ref()
            .filter(|p| p.defaults == defaults);
        let preferred_model = saved
            .map(|p| p.model.as_str())
            .or_else(|| defaults.get("model").and_then(|v| v.as_str()));
        let model = models
            .iter()
            .find(|m| Some(m.id.as_str()) == preferred_model)
            .or_else(|| {
                models
                    .iter()
                    .find(|m| defaults.get("model").and_then(|v| v.as_str()) == Some(m.id.as_str()))
            })
            .unwrap_or(&models[0])
            .id
            .clone();
        let thinking = saved
            .map(|p| p.thinking.clone())
            .or_else(|| {
                defaults
                    .get("thinking")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        self.model_selection = Some(ModelSelection {
            models,
            model,
            thinking,
            defaults,
        });
        self.normalize_thinking();
        Ok(())
    }

    fn normalize_thinking(&mut self) {
        let Some(selection) = self.model_selection.as_mut() else {
            return;
        };
        let Some(model) = selection.models.iter().find(|m| m.id == selection.model) else {
            return;
        };
        if !model.thinking_levels.contains(&selection.thinking) {
            selection.thinking = model
                .thinking_levels
                .iter()
                .find(|v| *v == "high")
                .or_else(|| model.thinking_levels.first())
                .cloned()
                .unwrap_or_default();
        }
    }

    pub fn select_model(&mut self, model: Option<&str>, thinking: Option<&str>) -> Result<()> {
        let selection = self
            .model_selection
            .as_mut()
            .ok_or_else(|| anyhow!("Connect to 雪小米 before selecting a model."))?;
        let chosen = model.unwrap_or(&selection.model);
        let option = selection
            .models
            .iter()
            .find(|m| m.id == chosen)
            .ok_or_else(|| anyhow!("The gateway no longer offers model {chosen}."))?;
        if let Some(level) = thinking {
            if !option.thinking_levels.iter().any(|v| v == level) {
                return Err(anyhow!(
                    "The gateway does not offer thinking level {level} for {chosen}."
                ));
            }
        }
        selection.model = chosen.to_string();
        if let Some(thinking) = thinking {
            selection.thinking = thinking.to_string();
        }
        self.normalize_thinking();
        self.model_preference = self
            .model_selection
            .as_ref()
            .map(ModelSelection::preference);
        self.emit_model_selection(None);
        Ok(())
    }

    pub(super) fn restore_session_model(&mut self, session_id: &str) {
        let Some((model, thinking)) = self.session_models.get(session_id) else {
            return;
        };
        let Some(selection) = self.model_selection.as_mut() else {
            return;
        };
        if !selection.models.iter().any(|m| &m.id == model) {
            return;
        }
        selection.model = model.clone();
        if let Some(thinking) = thinking {
            selection.thinking = thinking.clone();
        }
        self.normalize_thinking();
        self.emit_model_selection(Some(session_id.to_string()));
    }

    pub(super) fn emit_model_selection(&self, session_id: Option<String>) {
        if let (Some(events), Some(selection)) =
            (self.shared().events.clone(), self.model_selection.clone())
        {
            let _ = events.unbounded_send(AgentEvent::ModelSelection {
                session_id,
                selection,
            });
        }
    }
}
