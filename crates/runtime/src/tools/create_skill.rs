//! Инструмент create_skill: агент создаёт себе новый скилл (FR-051).
//!
//! Пишет SKILL.md в каталог скиллов, строго валидирует (D1), регистрирует.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::skills_validate::validate_generated;

use super::{Tool, ToolContext, ToolOutput};

pub struct CreateSkill;

#[async_trait]
impl Tool for CreateSkill {
    fn name(&self) -> &'static str {
        "create_skill"
    }
    fn description(&self) -> &'static str {
        "Создать новый скилл. Принимает name, description и body (инструкции). \
         Валидирует строго и сохраняет в .vpsagent/skills/. Доступен ТОЛЬКО главному агенту."
    }
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Имя скилла (латиница, дефис/подчёркивание)." },
                "description": { "type": "string", "description": "Описание (для вызова)." },
                "body": { "type": "string", "description": "Инструкции скилла (markdown с секциями ##)." }
            },
            "required": ["name", "description", "body"]
        })
    }
    async fn run(&self, input: Value, ctx: &ToolContext) -> ToolOutput {
        let name = input.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let description = input.get("description").and_then(|v| v.as_str()).unwrap_or("");
        let body = input.get("body").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() || description.is_empty() || body.is_empty() {
            return ToolOutput::err("нужны name, description и body");
        }

        // Строим скилл и строго валидируем.
        let content = crate::skills_validate::template(name, description, body);
        let skill = match crate::skills::Skill::parse(&content, PathBuf::from("<generated>")) {
            Ok(s) => s,
            Err(e) => return ToolOutput::err(format!("не удалось распарсить: {e}")),
        };
        let v = validate_generated(&skill);
        if !v.valid {
            return ToolOutput::err(format!("валидация не пройдена: {}", v.errors.join("; ")));
        }

        // Сохраняем в каталог скиллов сессии.
        let skills_dir = ctx.cwd.join(".vpsagent/skills");
        if let Err(e) = std::fs::create_dir_all(&skills_dir) {
            return ToolOutput::err(format!("mkdir: {e}"));
        }
        let path = skills_dir.join(format!("{name}.md"));
        if let Err(e) = std::fs::write(&path, &content) {
            return ToolOutput::err(format!("write: {e}"));
        }
        ToolOutput::ok(format!("Скилл '{name}' создан и валиден: {}", path.display()))
    }
}