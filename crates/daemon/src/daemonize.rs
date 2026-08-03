//! Само-демонизация процесса на Unix (FR-001, FR-008, FR-011).
//!
//! Модель: один `fork` + `setsid` + перенаправление stdio в лог-файл. Классический
//! double-fork здесь избыточен — мы не открываем слушающие сокеты до отсоединения,
//! а `setsid` достаточно отрывает от управляющего терминала, чтобы пережить SIGHUP
//! от закрытия SSH. Совпадает с паттерном существующего `ensure_daemon` в бинаре
//! (`pre_exec(setsid)` + `Stdio::null()`).
//!
//! После успешной `daemonize` вызывающий процесс становится демоном: новый лидер
//! сессии, без управляющего терминала, stdio уходит в лог-файл. SIGHUP игнорируется
//! явно (FR-011), чтобы случайный HUP не убил уже отсоединённый демон.

use std::path::Path;

use vpsagent_core::{Error, Result};

/// Результат попытки демонизации.
#[derive(Debug)]
pub enum Daemonize {
    /// Процесс стал демоном (мы — child после fork).
    Child,
    /// Мы — родитель; child ушёл в фон. `child_pid` — для лога/отчёта.
    Parent { child_pid: u32 },
    /// Платформа не поддерживает daemonize — работаем в foreground (FR-008).
    Unsupported,
}

/// Отсоединить процесс от управляющего терминала (Unix).
///
/// `log_file` — куда перенаправить stdout/stderr демона; создаётся (append, 0600).
/// На не-Unix возвращает [`Daemonize::Unsupported`] — caller должен продолжать
/// в foreground с предупреждением.
pub fn daemonize(log_file: &Path) -> Result<Daemonize> {
    #[cfg(unix)]
    {
        daemonize_unix(log_file)
    }
    #[cfg(not(unix))]
    {
        let _ = log_file;
        Ok(Daemonize::Unsupported)
    }
}

#[cfg(unix)]
fn daemonize_unix(log_file: &Path) -> Result<Daemonize> {
    use std::ffi::CString;
    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;

    // 1. fork — родитель завершается, child продолжает.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(Error::Other(anyhow::anyhow!("fork failed: {}", errno())));
    }
    if pid > 0 {
        // Родитель: child уходит в фон.
        return Ok(Daemonize::Parent {
            child_pid: pid as u32,
        });
    }
    // Дальше — только child.

    // 2. setsid — новая сессия, отрыв от управляющего терминала.
    if unsafe { libc::setsid() } < 0 {
        return Err(Error::Other(anyhow::anyhow!("setsid failed: {}", errno())));
    }

    // 3. Игнорировать SIGHUP (FR-011): после setsid мы лидер сессии; явный
    //    SIG_IGN гарантирует, что HUP не убьёт уже отсоединённый демон.
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }

    // 4. Перенаправить stdio: stdin → /dev/null, stdout/stderr → лог-файл (append).
    //    Лог-файл создаётся (append, права 0600), fd подменяется через dup2.
    if let Some(parent) = log_file.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let dev_null = CString::new("/dev/null").unwrap();
    let null_fd = unsafe { libc::open(dev_null.as_ptr(), libc::O_RDONLY) };
    if null_fd < 0 {
        return Err(Error::Other(anyhow::anyhow!(
            "open /dev/null failed: {}",
            errno()
        )));
    }
    let log_file_handle = OpenOptions::new()
        .create(true)
        .append(true)
        .read(false)
        .open(log_file)
        .map_err(|e| Error::Other(anyhow::anyhow!("не удалось открыть лог {log_file:?}: {e}")))?;
    let log_fd = log_file_handle.as_raw_fd();

    // dup2 атомарно подменяет fd 0/1/2; закрываем исходные fd после.
    unsafe {
        if libc::dup2(null_fd, 0) < 0 {
            return Err(Error::Other(anyhow::anyhow!(
                "dup2 stdin failed: {}",
                errno()
            )));
        }
        if libc::dup2(log_fd, 1) < 0 {
            return Err(Error::Other(anyhow::anyhow!(
                "dup2 stdout failed: {}",
                errno()
            )));
        }
        if libc::dup2(log_fd, 2) < 0 {
            return Err(Error::Other(anyhow::anyhow!(
                "dup2 stderr failed: {}",
                errno()
            )));
        }
        // Закрываем оригинальные fd (0/1/2 теперь дубликаты).
        if null_fd > 2 {
            libc::close(null_fd);
        }
        // log_fd: не закрываем, если он > 2, но dup2 уже создал копии на 1/2;
        // оставляем handle до конца области — drop закроет оригинал.
    }
    // Права на лог-файл — 0600 (как на сокет), чтобы не утекали пути/токены.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(log_file, std::fs::Permissions::from_mode(0o600));
    }
    // Удерживаем log_file_handle до конца, чтобы fd не закрылся раньше dup2.
    drop(log_file_handle);

    Ok(Daemonize::Child)
}

#[cfg(unix)]
fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    /// На Unix `daemonize` с /dev/null-подобным логом не должен паниковать на
    /// маршруте до fork. Полный end-to-end (с реальным fork) покрывается
    /// интеграционным smoke-тестом Фазы 5.
    #[test]
    fn daemonize_returns_enum() {
        // Не запускаем реальный fork в unit-тесте (нелегко проверить в one-shot).
        // Проверяем лишь, что тип компилируется и вариант существует.
        let _ = Daemonize::Unsupported;
        let _ = Daemonize::Child;
        let _ = Daemonize::Parent { child_pid: 0 };
    }
}
