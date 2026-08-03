//! Singleton-lock демона: pid-файл с атомарной публикацией (C12).
//!
//! Проблема наивной схемы: create_new + write неатомарны — между ними файл
//! пуст, и конкурент может принять его за призрак и удалить чужой lock.
//! Здесь: pid пишется во временный файл `{path}.tmp.{pid}`, затем атомарный
//! hard link в целевой путь (link(2) не перезаписывает существующий файл —
//! это одновременно и арбитраж "кто первый", и атомарная публикация контента).
//! На Linux при падении процесса файл остаётся — клиент при подключении
//! проверяет жив ли pid (kill 0).

use std::fs::File;
use std::io::Write;
use std::path::Path;

use vpsagent_core::Result;

/// Хранит pid-файл; при drop — удаляет.
pub struct DaemonLock {
    pid: u32,
    path: std::path::PathBuf,
}

impl DaemonLock {
    /// Попытаться захватить lock. Ok(Some) — мы демон; Ok(None) — занято другим.
    pub fn acquire(path: &Path) -> Result<Option<Self>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // Если файл существует — проверим, жив ли процесс.
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(path) {
                if let Ok(pid) = content.trim().parse::<i32>() {
                    // kill(pid, 0) возвращает Ok, если процесс жив (Unix).
                    let alive = libc_kill(pid, 0) == 0;
                    if alive {
                        return Ok(None); // демон уже работает
                    }
                }
            }
            // Мёртвый pid / битый контент — удаляем файл-призрак.
            // Пустого файла при атомарной публикации не бывает (см. ниже).
            std::fs::remove_file(path).ok();
        }
        let pid = std::process::id();
        // 1. pid во временный файл (уникальный на процесс — без гонок по tmp).
        let tmp = path.with_extension(format!("tmp.{pid}"));
        {
            let mut f = File::create(&tmp)?;
            write!(f, "{pid}")?;
            f.sync_all().ok();
        }
        // 2. Атомарный hard link: не перезаписывает существующий целевой файл.
        //    Кто слинковал первым — тот демон; окна с пустым файлом нет.
        let won = std::fs::hard_link(&tmp, path).is_ok();
        std::fs::remove_file(&tmp).ok();
        if !won {
            return Ok(None); // кто-то успел раньше
        }
        Ok(Some(Self {
            pid,
            path: path.to_path_buf(),
        }))
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).ok();
    }
}

/// Безопасная обёртка над libc::kill (без зависимости от libc-crate).
/// На Unix `kill(pid, 0)` проверяет существование процесса.
fn libc_kill(pid: i32, sig: i32) -> i32 {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid, sig) }
}
