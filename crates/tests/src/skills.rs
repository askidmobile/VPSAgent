//! Тесты скиллов: парсер SKILL.md, строгий валидатор, create_skill.

use vpsagent_runtime::{Skill, SkillRegistry, validate_generated};

#[test]
fn parse_skill_with_frontmatter() {
    let md = "\
---
name: deploy
description: Деплой на сервер
---
1. Собери проект
2. Скопируй на сервер";
    let skill = Skill::parse(md, std::path::PathBuf::from("deploy.md")).unwrap();
    assert_eq!(skill.name, "deploy");
    assert!(skill.description.contains("Деплой"));
    assert!(skill.body.contains("Собери проект"));
}

#[test]
fn parse_skill_missing_name_fails() {
    let md = "\
---
description: нет имени
---
тело";
    let err = Skill::parse(md, std::path::PathBuf::from("x.md")).unwrap_err();
    assert!(err.to_string().contains("name"), "ошибка должна упоминать name: {err}");
}

#[test]
fn load_skills_from_directory() {
    let dir = std::env::temp_dir().join(format!("vp-skills-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::write(
        dir.join("a.md"),
        "---\nname: alpha\ndescription: скилл альфа\n---\nделай альфа",
    )
    .unwrap();
    std::fs::write(
        dir.join("nested/SKILL.md"),
        "---\nname: nested-skill\ndescription: вложенный\n---\nделай бета",
    )
    .unwrap();
    let reg = SkillRegistry::load_from(&[dir.clone()]);
    assert!(
        reg.get("alpha").is_some(),
        "alpha должен загрузиться из .md"
    );
    assert!(
        reg.get("nested-skill").is_some(),
        "nested-skill должен загрузиться из вложенного SKILL.md"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn validate_generated_accepts_good() {
    let md = "\
---
name: deploy
description: Полный деплой проекта на сервер по SSH
---
## Инструкции

1. Собери release-бинарь.
2. Скопируй по scp на сервер.
3. Перезапусти systemd-сервис.
";
    let skill = Skill::parse(md, std::path::PathBuf::from("gen.md")).unwrap();
    let v = validate_generated(&skill);
    assert!(v.valid, "валидный скилл должен пройти: {:?}", v.errors);
}

#[test]
fn validate_generated_rejects_short_body() {
    let md = "\
---
name: deploy
description: Полный деплой проекта на сервер по SSH
---
тело";
    let skill = Skill::parse(md, std::path::PathBuf::from("gen.md")).unwrap();
    let v = validate_generated(&skill);
    assert!(!v.valid, "короткое тело без секций должно падать");
    assert!(!v.errors.is_empty());
}

#[test]
fn validate_generated_rejects_bad_name() {
    let md = "\
---
name: 'deploy !!bad'
description: Полный деплой проекта на сервер по SSH
---
## Инструкции

тело с достаточной длиной для валидации и наличием секции
";
    let skill = Skill::parse(md, std::path::PathBuf::from("gen.md")).unwrap();
    let v = validate_generated(&skill);
    assert!(!v.valid, "имя с пробелами/символами должно падать");
}