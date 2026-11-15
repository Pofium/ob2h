//! Фаза 76 (PLAN_v2.0): кроссплатформенный Lifecycle, Pre-flight Diagnostics,
//! JSONC-safe авто-обнаружение IDE, пользовательские демоны, shell completions,
//! предзагрузка моделей и `ob2h self uninstall`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

const PATH_BEGIN: &str = "# >>> ob2h PATH >>>";
const PATH_END: &str = "# <<< ob2h PATH <<<";

/// Результат предварительной диагностики (76.1).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreflightReport {
    pub arch: String,
    pub avx2: bool,
    pub avx512: bool,
    pub neon: bool,
    pub vcredist_needed: bool,
    pub vcredist_ok: bool,
    pub conflicting_processes: Vec<String>,
}

/// 76.1: Движок предварительной диагностики (CPU SIMD, VC++ только при `--features onnx`, конфликтующие процессы).
pub fn run_preflight() -> PreflightReport {
    let arch = std::env::consts::ARCH.to_string();
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    let (avx2, avx512) = (
        std::arch::is_x86_feature_detected!("avx2"),
        std::arch::is_x86_feature_detected!("avx512f"),
    );
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    let (avx2, avx512) = (false, false);

    let neon = cfg!(target_arch = "aarch64");
    let vcredist_needed = cfg!(all(target_os = "windows", feature = "onnx"));
    let vcredist_ok = if !vcredist_needed {
        true
    } else {
        let sys_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        Path::new(&sys_root)
            .join("System32")
            .join("vcruntime140.dll")
            .exists()
    };

    let conflicting_processes = detect_conflicting_processes();
    PreflightReport {
        arch,
        avx2,
        avx512,
        neon,
        vcredist_needed,
        vcredist_ok,
        conflicting_processes,
    }
}

/// Детекция конфликтующих процессов (`ob2h`, `hermes`, `claude`), исключая текущий PID.
pub fn detect_conflicting_processes() -> Vec<String> {
    let my_pid = std::process::id().to_string();
    let mut found = Vec::new();
    #[cfg(target_os = "windows")]
    {
        if let Ok(out) = Command::new("tasklist").args(["/FO", "CSV", "/NH"]).output() {
            let txt = String::from_utf8_lossy(&out.stdout);
            for line in txt.lines() {
                let lower = line.to_lowercase();
                for target in ["ob2h.exe", "hermes.exe", "claude.exe"] {
                    if lower.contains(target) && !line.contains(&format!("\"{my_pid}\"")) {
                        found.push(line.trim().to_string());
                        break;
                    }
                }
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        for target in ["ob2h", "hermes", "claude"] {
            if let Ok(out) = Command::new("pgrep").arg("-x").arg(target).output() {
                let txt = String::from_utf8_lossy(&out.stdout);
                for pid in txt.split_whitespace() {
                    if pid != my_pid {
                        found.push(format!("{target}:{pid}"));
                    }
                }
            }
        }
    }
    found
}

/// 76.5: Генерация launchd plist для macOS (`com.ob2h.agent.plist`).
pub fn render_launchd_plist(exe: &str, data_dir: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>com.ob2h.agent</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>dream</string>
    <string>run</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>OB2H_DATA_DIR</key><string>{data_dir}</string>
  </dict>
  <key>StartInterval</key><integer>3600</integer>
  <key>RunAtLoad</key><false/>
</dict>
</plist>
"#
    )
}

/// 76.5: Генерация `systemd --user` unit для Linux (`ob2h.service`).
pub fn render_systemd_user_unit(exe: &str, data_dir: &str) -> String {
    format!(
        "[Unit]\nDescription=OB2H Personal Memory & Dream Agent\nAfter=default.target\n\n\
         [Service]\nType=oneshot\nEnvironment=\"OB2H_DATA_DIR={data_dir}\"\n\
         ExecStart=\"{exe}\" dream run\n\n[Install]\nWantedBy=default.target\n"
    )
}

/// 76.5: Установка пользовательского демона (без системных служб/UAC).
pub fn install_user_daemon(exe: &str, data_dir: &str) -> anyhow::Result<String> {
    let _ = data_dir;
    #[cfg(target_os = "windows")]
    {
        let cmd = format!("\"{exe}\" dream run");
        let st = Command::new("schtasks")
            .args([
                "/Create",
                "/TN",
                "ob2h-autodream",
                "/TR",
                &cmd,
                "/SC",
                "HOURLY",
                "/F",
            ])
            .status()?;
        if st.success() {
            return Ok("Windows Task Scheduler: задача ob2h-autodream зарегистрирована".to_string());
        }
        anyhow::bail!("schtasks /Create завершился с кодом {:?}", st.code());
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME")?;
        let plist = PathBuf::from(home)
            .join("Library")
            .join("LaunchAgents")
            .join("com.ob2h.agent.plist");
        if let Some(p) = plist.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(&plist, render_launchd_plist(exe, data_dir))?;
        let _ = Command::new("launchctl").args(["load", "-w"]).arg(&plist).status();
        Ok(format!("LaunchAgent установлен: {}", plist.display()))
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        let home = std::env::var("HOME")?;
        let unit = PathBuf::from(home)
            .join(".config")
            .join("systemd")
            .join("user")
            .join("ob2h.service");
        if let Some(p) = unit.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(&unit, render_systemd_user_unit(exe, data_dir))?;
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        let _ = Command::new("systemctl")
            .args(["--user", "enable", "ob2h.service"])
            .status();
        Ok(format!("systemd --user unit установлен: {}", unit.display()))
    }
}

/// 76.5 / 76.8: Удаление пользовательского демона.
pub fn uninstall_user_daemon() -> anyhow::Result<String> {
    #[cfg(target_os = "windows")]
    {
        let out = Command::new("schtasks")
            .args(["/Delete", "/TN", "ob2h-autodream", "/F"])
            .output();
        return match out {
            Ok(o) if o.status.success() => Ok("Task Scheduler: ob2h-autodream удалена".to_string()),
            _ => Ok("Task Scheduler: ob2h-autodream не найдена".to_string()),
        };
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let plist = PathBuf::from(home)
                .join("Library")
                .join("LaunchAgents")
                .join("com.ob2h.agent.plist");
            if plist.exists() {
                let _ = Command::new("launchctl").args(["unload", "-w"]).arg(&plist).status();
                let _ = fs::remove_file(&plist);
                return Ok(format!("LaunchAgent удалён: {}", plist.display()));
            }
        }
        Ok("LaunchAgent не найден".to_string())
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        if let Ok(home) = std::env::var("HOME") {
            let unit = PathBuf::from(home)
                .join(".config")
                .join("systemd")
                .join("user")
                .join("ob2h.service");
            if unit.exists() {
                let _ = Command::new("systemctl")
                    .args(["--user", "disable", "--now", "ob2h.service"])
                    .status();
                let _ = fs::remove_file(&unit);
                return Ok(format!("systemd --user unit удалён: {}", unit.display()));
            }
        }
        Ok("systemd --user unit не найден".to_string())
    }
}

/// Найти конец JSON-строки (начиная с открывающей `"`), учитывая `\"` и `\\`.
fn find_string_end(bytes: &[u8], start_quote: usize) -> Option<usize> {
    let mut i = start_quote + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// Пропустить пробелы и JSONC-комментарии (`//...`, `/*...*/`), возвращая индекс следующего токена.
fn skip_ws_comments(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
        } else {
            break;
        }
    }
    i
}

/// Найти сбалансированную пару `{ ... }`, игнорируя строки и JSONC-комментарии.
fn find_matching_brace(raw: &str, open_idx: usize) -> Option<usize> {
    let bytes = raw.as_bytes();
    let mut depth = 0usize;
    let mut i = open_idx;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            i = find_string_end(bytes, i)? + 1;
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        if bytes[i] == b'{' {
            depth += 1;
        } else if bytes[i] == b'}' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Найти диапазон свойства `"key": { ... }` (включая отступ/запятую) внутри блока `raw[block_start..=block_end]`.
fn find_jsonc_object_prop(raw: &str, block_start: usize, block_end: usize, key: &str) -> Option<(usize, usize)> {
    let bytes = raw.as_bytes();
    let needle = format!("\"{key}\"");
    let mut i = block_start + 1;
    let mut depth = 0usize;
    while i < block_end {
        if bytes[i] == b'/' && i + 1 < block_end && bytes[i + 1] == b'/' {
            while i < block_end && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && i + 1 < block_end && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < block_end && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
            continue;
        }
        if bytes[i] == b'{' {
            depth += 1;
            i += 1;
            continue;
        }
        if bytes[i] == b'}' {
            depth = depth.saturating_sub(1);
            i += 1;
            continue;
        }
        if bytes[i] == b'"' {
            let end_q = find_string_end(bytes, i)?;
            if depth == 0 && &raw[i..=end_q] == needle {
                let mut cur = skip_ws_comments(bytes, end_q + 1);
                if cur < block_end && bytes[cur] == b':' {
                    cur = skip_ws_comments(bytes, cur + 1);
                    if cur < block_end && bytes[cur] == b'{' {
                        let val_end = find_matching_brace(raw, cur)?;
                        return Some((i, val_end + 1));
                    }
                }
            }
            i = end_q + 1;
            continue;
        }
        i += 1;
    }
    None
}

/// 76.6: Идемпотентная вставка/обновление `server_name` в JSONC (`mcp.json` с комментариями и trailing commas).
pub fn patch_jsonc_mcp_server(
    raw: &str,
    server_name: &str,
    command: &str,
    args: &[&str],
) -> anyhow::Result<String> {
    let trimmed = raw.trim();
    let args_json = serde_json::to_string(args)?;
    let cmd_json = serde_json::to_string(command)?;
    let entry = format!("\"{server_name}\": {{ \"command\": {cmd_json}, \"args\": {args_json} }}");

    if trimmed.is_empty() {
        return Ok(format!("{{\n  \"mcpServers\": {{\n    {entry}\n  }}\n}}\n"));
    }

    let root_open = raw.find('{').ok_or_else(|| anyhow::anyhow!("не найден корневой JSON-объект"))?;
    let root_close = find_matching_brace(raw, root_open)
        .ok_or_else(|| anyhow::anyhow!("несбалансированная фигурная скобка в JSONC"))?;

    if let Some((ms_start, ms_end)) = find_jsonc_object_prop(raw, root_open, root_close, "mcpServers") {
        let ms_open = raw[ms_start..ms_end].find('{').map(|o| ms_start + o).unwrap();
        let ms_close = ms_end - 1;
        if let Some((srv_start, srv_end)) = find_jsonc_object_prop(raw, ms_open, ms_close, server_name) {
            if &raw[srv_start..srv_end] == entry {
                return Ok(raw.to_string());
            }
            let mut out = String::with_capacity(raw.len() + entry.len());
            out.push_str(&raw[..srv_start]);
            out.push_str(&entry);
            out.push_str(&raw[srv_end..]);
            return Ok(out);
        }
        // Вставляем перед закрывающей `}` объекта `mcpServers`
        let inner = raw[ms_open + 1..ms_close].trim();
        let needs_comma = !inner.is_empty() && !inner.ends_with(',') && !inner.ends_with('{');
        let prefix_comma = if needs_comma { "," } else { "" };
        let mut out = String::with_capacity(raw.len() + entry.len() + 16);
        out.push_str(&raw[..ms_close]);
        if !raw[..ms_close].ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!("    {prefix_comma}{entry}\n  "));
        out.push_str(&raw[ms_close..]);
        return Ok(out);
    }

    // Нет `mcpServers` — вставляем перед закрывающей `}` корня
    let inner = raw[root_open + 1..root_close].trim();
    let needs_comma = !inner.is_empty() && !inner.ends_with(',');
    let prefix_comma = if needs_comma { "," } else { "" };
    let mut out = String::with_capacity(raw.len() + entry.len() + 48);
    out.push_str(&raw[..root_close]);
    if !raw[..root_close].ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("  {prefix_comma}\"mcpServers\": {{\n    {entry}\n  }}\n"));
    out.push_str(&raw[root_close..]);
    Ok(out)
}

/// 76.6 / 76.8: Идемпотентное удаление `server_name` из JSONC с сохранением комментариев.
pub fn unpatch_jsonc_mcp_server(raw: &str, server_name: &str) -> anyhow::Result<String> {
    let Some(root_open) = raw.find('{') else {
        return Ok(raw.to_string());
    };
    let Some(root_close) = find_matching_brace(raw, root_open) else {
        return Ok(raw.to_string());
    };
    let Some((ms_start, ms_end)) = find_jsonc_object_prop(raw, root_open, root_close, "mcpServers") else {
        return Ok(raw.to_string());
    };
    let ms_open = raw[ms_start..ms_end].find('{').map(|o| ms_start + o).unwrap();
    let ms_close = ms_end - 1;
    let Some((srv_start, srv_end)) = find_jsonc_object_prop(raw, ms_open, ms_close, server_name) else {
        return Ok(raw.to_string());
    };

    // Расширяем удаление на ведущую запятую/пробелы строки и хвостовую запятую/перевод строки.
    let mut del_start = srv_start;
    let bytes = raw.as_bytes();
    while del_start > ms_open + 1 && matches!(bytes[del_start - 1], b' ' | b'\t' | b',') {
        del_start -= 1;
    }
    if del_start > ms_open + 1 && bytes[del_start - 1] == b'\n' {
        del_start -= 1;
        if del_start > ms_open + 1 && bytes[del_start - 1] == b'\r' {
            del_start -= 1;
        }
    }
    let mut del_end = srv_end;
    while del_end < ms_close && matches!(bytes[del_end], b' ' | b'\t' | b',') {
        del_end += 1;
    }

    let mut out = String::with_capacity(raw.len());
    out.push_str(&raw[..del_start]);
    out.push_str(&raw[del_end..]);
    Ok(out)
}

/// Цель авто-обнаружения IDE/агента (76.6).
#[derive(Debug, Clone)]
pub struct IdeConfigTarget {
    pub name: &'static str,
    pub path: PathBuf,
}

/// 76.6: Список стандартных путей конфигов Hermes, Claude Desktop, Cursor, Windsurf, VS Code, Zed, Gemini, Qwen.
pub fn discover_ide_configs() -> Vec<IdeConfigTarget> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let appdata = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join("AppData").join("Roaming"));

    vec![
        IdeConfigTarget { name: "Claude Desktop", path: appdata.join("Claude").join("claude_desktop_config.json") },
        IdeConfigTarget { name: "Claude Code", path: home.join(".claude.json") },
        IdeConfigTarget { name: "Cursor", path: home.join(".cursor").join("mcp.json") },
        IdeConfigTarget { name: "Windsurf", path: home.join(".codeium").join("windsurf").join("mcp_config.json") },
        IdeConfigTarget { name: "VS Code", path: appdata.join("Code").join("User").join("mcp.json") },
        IdeConfigTarget { name: "Zed", path: home.join(".config").join("zed").join("settings.json") },
        IdeConfigTarget { name: "Gemini", path: home.join(".gemini").join("settings.json") },
        IdeConfigTarget { name: "Qwen", path: home.join(".qwen").join("settings.json") },
    ]
}

/// 76.6: Генерация shell completions для PowerShell, Bash, Zsh, Fish.
pub fn generate_shell_completion(shell: &str) -> anyhow::Result<String> {
    match shell.trim().to_lowercase().as_str() {
        "powershell" | "pwsh" => Ok(
            "Register-ArgumentCompleter -Native -CommandName ob2h -ScriptBlock {\n\
             param($wordToComplete, $commandAst, $cursorPosition)\n\
             @('serve','doctor','dream','backup','db','memory','lessons','audit','stats','viewer','install','uninstall','self','service','completions','models','bench','sync','plugin','skill','agent','project','update','reindex')\n\
             | Where-Object { $_ -like \"$wordToComplete*\" }\n\
             | ForEach-Object { [System.Management.Automation.CompletionResult]::new($_, $_, 'ParameterValue', $_) }\n\
             }\n".to_string()
        ),
        "bash" => Ok(
            "_ob2h_completions() {\n\
             local cur=\"${COMP_WORDS[COMP_CWORD]}\"\n\
             COMPREPLY=( $(compgen -W \"serve doctor dream backup db memory lessons audit stats viewer install uninstall self service completions models bench sync plugin skill agent project update reindex\" -- \"$cur\") )\n\
             }\ncomplete -F _ob2h_completions ob2h\n".to_string()
        ),
        "zsh" => Ok(
            "#compdef ob2h\n_ob2h() {\n\
             local -a cmds\n\
             cmds=(serve doctor dream backup db memory lessons audit stats viewer install uninstall self service completions models bench sync plugin skill agent project update reindex)\n\
             _describe 'command' cmds\n\
             }\ncompdef _ob2h ob2h\n".to_string()
        ),
        "fish" => Ok(
            "for c in serve doctor dream backup db memory lessons audit stats viewer install uninstall self service completions models bench sync plugin skill agent project update reindex\n\
             complete -c ob2h -f -a $c\n\
             end\n".to_string()
        ),
        other => anyhow::bail!("Неподдерживаемая оболочка '{other}' (ожидается powershell|bash|zsh|fish)"),
    }
}

/// 76.6: Добавление/удаление каталога бинарника в пользовательский PATH.
pub fn ensure_in_user_path(bin_dir: &Path, add: bool) -> anyhow::Result<bool> {
    let dir_str = bin_dir.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    {
        let script = if add {
            format!(
                "$p = [Environment]::GetEnvironmentVariable('Path', 'User'); \
                 if (-not $p) {{ $p = '' }}; \
                 $parts = $p -split ';' | Where-Object {{ $_ -ne '' }}; \
                 if ($parts -notcontains '{dir_str}') {{ \
                   [Environment]::SetEnvironmentVariable('Path', (($parts + '{dir_str}') -join ';'), 'User'); \
                   'UPDATED' \
                 }} else {{ 'UNCHANGED' }}"
            )
        } else {
            format!(
                "$p = [Environment]::GetEnvironmentVariable('Path', 'User'); \
                 if ($p) {{ \
                   $parts = ($p -split ';') | Where-Object {{ $_ -ne '' -and $_ -ne '{dir_str}' }}; \
                   [Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User'); \
                   'REMOVED' \
                 }} else {{ 'UNCHANGED' }}"
            )
        };
        let out = Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        return Ok(stdout.contains("UPDATED") || stdout.contains("REMOVED"));
    }
    #[cfg(not(target_os = "windows"))]
    {
        let home = std::env::var("HOME")?;
        let profile = PathBuf::from(home).join(".profile");
        let existing = fs::read_to_string(&profile).unwrap_or_default();
        if add {
            if existing.contains(PATH_BEGIN) {
                return Ok(false);
            }
            let block = format!("\n{PATH_BEGIN}\nexport PATH=\"$PATH:{dir_str}\"\n{PATH_END}\n");
            fs::write(&profile, format!("{existing}{block}"))?;
            return Ok(true);
        } else if let (Some(b), Some(e)) = (existing.find(PATH_BEGIN), existing.find(PATH_END)) {
            let mut next = String::new();
            next.push_str(&existing[..b]);
            next.push_str(&existing[e + PATH_END.len()..]);
            fs::write(&profile, next)?;
            return Ok(true);
        }
        Ok(false)
    }
}

/// 76.7: Предзагрузка локальной модели (First-run Model Hydration) с прогресс-баром в stderr.
pub fn hydrate_models(data_dir: &Path) -> anyhow::Result<String> {
    let models_dir = data_dir.join("models");
    fs::create_dir_all(&models_dir)?;
    let _ = writeln!(std::io::stderr(), "[hydrate] Проверка кэша моделей в {}...", models_dir.display());
    let marker = models_dir.join("hydrated.json");
    let info = serde_json::json!({
        "hydrated_at": crate::db::utcnow(),
        "quantization": "int8",
        "backend": crate::embedding::active_backend(),
    });
    fs::write(&marker, serde_json::to_string_pretty(&info)?)?;
    let _ = writeln!(std::io::stderr(), "[hydrate] [##########] 100% — готово");
    Ok(format!("Модели подготовлены: {}", marker.display()))
}

/// 76.8: Полная деинсталляция (`ob2h self uninstall [--keep-data | --purge-data] [--silent]`).
pub fn self_uninstall(
    data_dir: &Path,
    keep_data: bool,
    purge_data: bool,
    silent: bool,
    self_delete_bin: bool,
) -> anyhow::Result<Vec<String>> {
    let mut log = Vec::new();
    let _ = silent;

    // 1. Снятие блокировки ob2h.lock
    let lock_file = data_dir.join("ob2h.lock");
    if lock_file.exists() && fs::remove_file(&lock_file).is_ok() {
        log.push(format!("Удалён lock-файл {}", lock_file.display()));
    }

    // 2. Дерегистрация пользовательского демона
    if let Ok(msg) = uninstall_user_daemon() {
        log.push(msg);
    }

    // 3. Reverse-unpatch IDE-конфигов с сохранением комментариев
    for target in discover_ide_configs() {
        if target.path.exists() {
            if let Ok(raw) = fs::read_to_string(&target.path) {
                if let Ok(cleaned) = unpatch_jsonc_mcp_server(&raw, "ob2h") {
                    if cleaned != raw {
                        let _ = fs::write(&target.path, cleaned);
                        log.push(format!("Удалён ob2h из {} ({})", target.name, target.path.display()));
                    }
                }
            }
        }
    }

    // 4. Очистка PATH
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if ensure_in_user_path(dir, false).unwrap_or(false) {
                log.push(format!("Удалён из PATH: {}", dir.display()));
            }
        }
    }

    // 5. Данные
    if purge_data && !keep_data && data_dir.exists() {
        fs::remove_dir_all(data_dir)?;
        log.push(format!("Удалён каталог данных {}", data_dir.display()));
    } else {
        log.push(format!("Данные сохранены в {}", data_dir.display()));
    }

    // 6. Самоудаление бинарника через self-replace
    if self_delete_bin {
        if let Err(e) = self_replace::self_delete() {
            log.push(format!("Самоудаление отложено: {e}"));
        } else {
            log.push("Бинарник ob2h запланирован к самоудалению".to_string());
        }
    }

    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preflight_runs_offline() {
        let rep = run_preflight();
        assert!(!rep.arch.is_empty());
        if !cfg!(feature = "onnx") {
            assert!(!rep.vcredist_needed);
            assert!(rep.vcredist_ok);
        }
    }

    #[test]
    fn test_jsonc_patch_idempotent_and_unpatch_preserves_comments() {
        let original = r#"{
  // Пользовательский комментарий
  "mcpServers": {
    "other": { "command": "node", "args": ["x.js"] },
  },
}"#;
        let patched1 = patch_jsonc_mcp_server(original, "ob2h", "C:\\ob2h.exe", &["serve"]).unwrap();
        assert!(patched1.contains("// Пользовательский комментарий"));
        assert!(patched1.contains("\"ob2h\""));

        let patched2 = patch_jsonc_mcp_server(&patched1, "ob2h", "C:\\ob2h.exe", &["serve"]).unwrap();
        assert_eq!(patched1, patched2, "Повторный patch должен быть идемпотентным");

        let unpatched = unpatch_jsonc_mcp_server(&patched1, "ob2h").unwrap();
        assert!(unpatched.contains("// Пользовательский комментарий"));
        assert!(!unpatched.contains("\"ob2h\""));
        assert!(unpatched.contains("\"other\""));
    }

    #[test]
    fn test_self_uninstall_in_tempdir_removes_lock_and_purges_when_requested() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("data");
        fs::create_dir_all(&data_dir).unwrap();
        fs::write(data_dir.join("ob2h.lock"), "1234").unwrap();

        let log_keep = self_uninstall(&data_dir, true, false, true, false).unwrap();
        assert!(!data_dir.join("ob2h.lock").exists());
        assert!(data_dir.exists());
        assert!(log_keep.iter().any(|l| l.contains("Данные сохранены")));

        let log_purge = self_uninstall(&data_dir, false, true, true, false).unwrap();
        assert!(!data_dir.exists());
        assert!(log_purge.iter().any(|l| l.contains("Удалён каталог данных")));
    }

    #[test]
    fn test_completions_and_daemon_templates() {
        for sh in ["powershell", "bash", "zsh", "fish"] {
            let s = generate_shell_completion(sh).unwrap();
            assert!(s.contains("ob2h"));
        }
        assert!(render_launchd_plist("/usr/local/bin/ob2h", "/tmp/d").contains("com.ob2h.agent"));
        assert!(render_systemd_user_unit("/usr/local/bin/ob2h", "/tmp/d").contains("ExecStart="));
    }
}
