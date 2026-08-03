//! Строгая валидация самогенерированных скиллов (D1).
//!
//! В отличие от толерантного loader'а (который принимает чужие скиллы),
//! скиллы, созданные агентом самим себе, проходят полную проверку:
//! name, description, обязательные секции, минимальный объём.

use crate::skills::Skill;

/// Результат валидации.
#[derive(Debug)]
pub struct Validation {
    pub valid: bool,
    pub errors: Vec<String>,
}

impl Validation {
    fn ok() -> Self {
        Self {
            valid: true,
            errors: vec![],
        }
    }
    #[allow(dead_code)]
    fn fail(err: String) -> Self {
        Self {
            valid: false,
            errors: vec![err],
        }
    }
}

/// Проверить сгенерированный скилл.
pub fn validate_generated(skill: &Skill) -> Validation {
    let mut errors = vec![];

    // name: буквенно-цифровой + дефис/подчёркивание, длина 2..=64.
    if skill.name.len() < 2 || skill.name.len() > 64 {
        errors.push(format!("имя '{}' должно быть 2..64 символов", skill.name));
    } else if !skill
        .name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        errors.push(format!(
            "имя '{}' содержит недопустимые символы",
            skill.name
        ));
    }

    // description: 10..=500 символов.
    if skill.description.len() < 10 || skill.description.len() > 500 {
        errors.push(format!(
            "описание скилла должно быть 10..500 символов, сейчас {}",
            skill.description.len()
        ));
    }

    // body: не пустое, минимум 50 символов, содержит хоть одну секцию.
    if skill.body.trim().is_empty() {
        errors.push("тело скилла пустое".into());
    } else if skill.body.len() < 50 {
        errors.push(format!(
            "тело слишком короткое: {} символов (<50)",
            skill.body.len()
        ));
    }

    // Инструкция о том, КАК вызывать (минимум одна секция ## или ## Trigger/When to Use).
    let has_section = skill.body.contains("## ") || skill.body.contains("# ");
    if !has_section {
        errors.push("в теле скилла нет ни одной секции (##)".into());
    }

    if errors.is_empty() {
        Validation::ok()
    } else {
        Validation {
            valid: false,
            errors,
        }
    }
}

/// Собрать стандартный шаблон SKILL.md (для самогенерации).
pub fn template(name: &str, description: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n")
}
