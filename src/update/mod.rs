//! Модуль автообновления OB2H (ADR-15).
//!
//! Источник: GitHub Releases API.
//! Частота проверок: по умолчанию раз в 24ч, гейт через kv:`update_last_check_ts`.
//! Поведение по умолчанию: только уведомлять в лог/stdout, применять — ручной `ob2h update apply`.
//!
//! Self-replace бинарника: кроссплатформенный крейт `self-replace`.
//! После замены: новый бинарник запускает `plugin install` + `skill install`
//! (внутри текущего процесса — перед выходом, вызываем новый бинарник).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context};
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::config::Settings;
use crate::db::Database;

/// Актив из GitHub Release (артефакт для скачивания).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub size: u64,
    #[serde(rename = "browser_download_url")]
    pub download_url: String,
    pub content_type: Option<String>,
}

/// Ответ GitHub Releases API (сокращённый — только нужные поля).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub name: Option<String>,
    pub body: Option<String>,
    pub draft: bool,
    pub prerelease: bool,
    pub published_at: Option<String>,
    pub assets: Vec<ReleaseAsset>,
}

/// Информация о релизе, подготовленная для отображения/применения.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    /// Тег без префикса v, например "1.7.1".
    pub version: String,
    /// Исходный тег как есть, например "v1.7.1".
    pub tag: String,
    /// Имя релиза (markdown-заголовок).
    pub title: Option<String>,
    /// Changelog-описание релиза.
    pub body: Option<String>,
    /// Когда опубликован (RFC3339).
    pub published_at: Option<String>,
    /// Путь к скачанному zip-артефакту (None — ещё не скачан).
    pub asset: Option<ReleaseAsset>,
}

/// Результат проверки обновлений.
pub enum CheckResult {
    /// Обновлений нет (последняя проверка была недавно или последняя версия совпадает).
    UpToDate {
        last_check_ts: Option<i64>,
        last_seen: Option<String>,
    },
    /// Доступна новая версия.
    NewAvailable {
        release: ReleaseInfo,
        /// Нужно ли уведомлять пользователя (true — первое обнаружение за интервал).
        notify: bool,
    },
    /// Ошибка сети/GitHub API — мы не роняем приложение, только логируем.
    FetchError(String),
}

/// Текущая версия бинарника из Cargo.toml (env!("CARGO_PKG_VERSION")).
pub fn current_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("CARGO_PKG_VERSION должен быть валидным semver")
}

/// Парсит тег релиза ("v1.7.1" или "1.7.1") в semver::Version.
fn parse_tag(tag: &str) -> Option<semver::Version> {
    let clean = tag.strip_prefix('v').unwrap_or(tag);
    semver::Version::parse(clean).ok()
}

/// Сравнивает текущую версию с тегом релиза: true если релиз новее.
pub fn is_version_newer(current: &semver::Version, tag: &str) -> bool {
    match parse_tag(tag) {
        Some(latest) => latest > *current,
        None => false,
    }
}

/// Выбирает релизный актив для Windows x86_64 (zip-архив с exe).
///
/// Подходящие имена: `ob2h-x86_64-pc-windows-msvc.zip`, `ob2h-windows-amd64.zip`,
/// `ob2h_1.7.1_windows_x86_64.zip` — достаточно совпадения "windows" + "x86_64|amd64" + ".zip".
pub fn pick_windows_asset(assets: &[ReleaseAsset]) -> Option<&ReleaseAsset> {
    assets.iter().find(|a| {
        let name = a.name.to_lowercase();
        name.ends_with(".zip")
            && (name.contains("x86_64") || name.contains("amd64"))
            && name.contains("windows")
    })
}

/// Выбирает Linux/другой актив по имени платформы (для кроссплатформенности будущего).
pub fn pick_native_asset(assets: &[ReleaseAsset]) -> Option<&ReleaseAsset> {
    if cfg!(windows) {
        pick_windows_asset(assets).or_else(|| {
            // Фолбэк: любой zip с ".exe" внутри нас не волнует — выбираем по .zip,
            // если не нашли windows-специфичный.
            assets.iter().find(|a| a.name.to_lowercase().ends_with(".zip"))
        })
    } else if cfg!(target_os = "linux") {
        assets.iter().find(|a| {
            let name = a.name.to_lowercase();
            (name.ends_with(".tar.gz") || name.ends_with(".tgz") || name.ends_with(".zip"))
                && (name.contains("x86_64") || name.contains("amd64"))
                && name.contains("linux")
        })
    } else if cfg!(target_os = "macos") {
        assets.iter().find(|a| {
            let name = a.name.to_lowercase();
            (name.ends_with(".tar.gz") || name.ends_with(".zip")) && name.contains("mac")
        })
    } else {
        assets.first()
    }
}

impl ReleaseInfo {
    pub fn from_gh_release(r: &GitHubRelease) -> Option<Self> {
        let version = parse_tag(&r.tag_name)?.to_string();
        let asset = pick_native_asset(&r.assets).cloned();
        Some(Self {
            version,
            tag: r.tag_name.clone(),
            title: r.name.clone(),
            body: r.body.clone(),
            published_at: r.published_at.clone(),
            asset,
        })
    }
}

/// Выполняет HTTP-запрос к GitHub Releases API и возвращает последний релиз.
///
/// - `channel = "stable"`: используем `/releases/latest` (только не-prerelease, не-draft)
/// - `channel = "nightly"`: первый из `/releases` (может быть prerelease)
pub async fn fetch_latest_release(
    client: &reqwest::Client,
    repo: &str,
    channel: &str,
) -> anyhow::Result<ReleaseInfo> {
    let user_agent = format!("ob2h/{} (+https://github.com/{repo})", env!("CARGO_PKG_VERSION"));
    let accept = "application/vnd.github+json";
    let api_root = "https://api.github.com";

    let release: GitHubRelease = if channel == "nightly" {
        let url = format!("{api_root}/repos/{repo}/releases?per_page=10");
        let releases: Vec<GitHubRelease> = client
            .get(&url)
            .header(reqwest::header::USER_AGENT, &user_agent)
            .header(reqwest::header::ACCEPT, accept)
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()?
            .json()
            .await?;
        releases
            .into_iter()
            .find(|r| !r.draft)
            .ok_or_else(|| anyhow!("В репозитории {repo} нет ни одного не-draft release"))?
    } else {
        // stable → latest
        let url = format!("{api_root}/repos/{repo}/releases/latest");
        client
            .get(&url)
            .header(reqwest::header::USER_AGENT, &user_agent)
            .header(reqwest::header::ACCEPT, accept)
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()?
            .json()
            .await?
    };

    ReleaseInfo::from_gh_release(&release)
        .ok_or_else(|| anyhow!("Не удалось распарсить тег релиза: {}", release.tag_name))
}

const KV_LAST_CHECK_TS: &str = "update_last_check_ts";
const KV_LAST_SEEN_TAG: &str = "update_last_seen_tag";
const KV_LAST_SEEN_VERSION: &str = "update_last_seen_version";

/// Проверка обновлений с daily-гейтом по kv-хранилищу.
///
/// - `force = true`: игнорируем интервал и идём в GitHub сразу (команда `update check`).
/// - `force = false`: уважаем `update_interval_hours` и возвращаем UpToDate/NewAvailable
///   без уведомления, если проверка была недавно.
pub async fn check_update(
    db: &Database,
    settings: &Settings,
    force: bool,
) -> CheckResult {
    if !settings.update_enabled {
        return CheckResult::UpToDate {
            last_check_ts: None,
            last_seen: None,
        };
    }

    let cur = current_version();
    let now_ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let interval_secs = (settings.update_interval_hours.max(1) * 3600) as i64;

    let last_check_ts = db
        .get_kv(KV_LAST_CHECK_TS)
        .ok()
        .flatten()
        .and_then(|s| s.parse::<i64>().ok());
    let last_seen_tag = db.get_kv(KV_LAST_SEEN_TAG).ok().flatten();
    let _last_seen_version = db.get_kv(KV_LAST_SEEN_VERSION).ok().flatten();

    // (1) Не-форс: недавно уже проверяли — возвращаем статус без сетевого запроса.
    if !force {
        if let Some(ts) = last_check_ts {
            if now_ts.saturating_sub(ts) < interval_secs {
                // Проверка была недавно: если мы уже видели новую версию — возвращаем её,
                // но notify=false (не заспамливаем при каждом старте serve).
                if let Some(tag) = &last_seen_tag {
                    if is_version_newer(&cur, tag) {
                        // У нас нет полного ReleaseInfo (только тег) — фолбэк:
                        // возвращаем UpToDate, чтобы не вводить в заблуждение.
                        // Пользователь может вручную вызвать `ob2h update check`.
                    }
                }
                return CheckResult::UpToDate {
                    last_check_ts: Some(ts),
                    last_seen: last_seen_tag,
                };
            }
        }
    }

    // (2) Идём в GitHub.
    let client = reqwest_client();
    match fetch_latest_release(&client, &settings.update_repo, &settings.update_channel).await {
        Ok(release) => {
            // Пишем в kv: время последней проверки и увиденный тег.
            let _ = db.set_kv(KV_LAST_CHECK_TS, &now_ts.to_string());
            let _ = db.set_kv(KV_LAST_SEEN_TAG, &release.tag);
            let _ = db.set_kv(KV_LAST_SEEN_VERSION, &release.version);

            if is_version_newer(&cur, &release.tag) {
                CheckResult::NewAvailable {
                    release,
                    notify: true,
                }
            } else {
                CheckResult::UpToDate {
                    last_check_ts: Some(now_ts),
                    last_seen: Some(release.tag),
                }
            }
        }
        Err(e) => {
            // Даже при ошибке сети пишем last_check, чтобы не долбиться в GitHub
            // каждую минуту при перезапусках serve.
            let _ = db.set_kv(KV_LAST_CHECK_TS, &now_ts.to_string());
            warn!("Не удалось проверить обновления: {e:#}");
            CheckResult::FetchError(format!("{e:#}"))
        }
    }
}

/// Строит reqwest-клиент с таймаутом и TLS как в llm/openai.rs.
pub fn reqwest_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(format!("ob2h/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Печатает уведомление о новой версии в eprintln (stdout/stderr) + tracing::info.
pub fn notify_user(release: &ReleaseInfo) {
    let cur = current_version();
    let published = release
        .published_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "—".to_string());
    let asset_info = match &release.asset {
        Some(a) => format!("{} ({} МБ)", a.name, (a.size + 524288) / 1048576),
        None => "—".to_string(),
    };
    let msg = format!(
        "💡 Обновление OB2H доступно: v{cur} → {} (опубликовано {})\n   \
         Артефакт: {asset_info}\n   \
         Установить командой: ob2h update apply",
        release.tag, published
    );
    eprintln!("{msg}");
    info!(%release.tag, %asset_info, "Доступна новая версия OB2H — используйте `ob2h update apply`");
}

/// Скачивает архив релиза, распаковывает, self-replace бинарника,
/// запускает новый бинарник с `plugin install` и `skill install`.
pub async fn download_and_apply(
    release: &ReleaseInfo,
    settings: &Settings,
) -> anyhow::Result<()> {
    let asset = release
        .asset
        .as_ref()
        .ok_or_else(|| anyhow!("Для релиза {} нет подходящего бинарного архива", release.tag))?;

    // (1) Скачиваем zip
    let tmp_dir = settings.tmp_dir();
    std::fs::create_dir_all(&tmp_dir)?;
    let zip_path = tmp_dir.join(format!("ob2h_release_{}.zip", release.version));
    let client = reqwest_client();
    info!(url = %asset.download_url, to = %zip_path.display(), "Скачиваем релиз");
    download_file(&client, &asset.download_url, &zip_path).await?;

    // (2) Распаковываем
    let unpack_dir = tmp_dir.join(format!("ob2h_unpack_{}", release.version));
    if unpack_dir.exists() {
        std::fs::remove_dir_all(&unpack_dir)?;
    }
    std::fs::create_dir_all(&unpack_dir)?;
    info!(from = %zip_path.display(), to = %unpack_dir.display(), "Распаковываем архив");
    extract_zip(&zip_path, &unpack_dir)?;

    // (3) Находим бинарник внутри архива
    let new_exe = find_binary(&unpack_dir)?;
    info!(%release.tag, new_bin = %new_exe.display(), "Новый бинарник найден");

    // (4) Обновляем плагин и скилл НОВЫМ бинарником (чтобы использовался новый код install)
    run_subcommand(&new_exe, &["plugin", "install"], "обновление плагина")?;
    run_subcommand(&new_exe, &["skill", "install"], "обновление скилла")?;

    // (5) Self-replace текущий бинарник → он заменится после завершения этого процесса.
    info!(target = %new_exe.display(), "Заменяем текущий бинарник (self-replace)");
    self_replace::self_replace(&new_exe).with_context(|| {
        format!(
            "self-replace {} → {}",
            new_exe.display(),
            std::env::current_exe()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "?".into())
        )
    })?;

    // (6) Чистимся: удаляем временные файлы архива (лучше усилием нового бинарника;
    // но текущий процесс завершается сразу — оставим напоминание в логе).
    eprintln!(
        "✅ Обновление до {} применено. Перезапустите ob2h для работы с новой версией.",
        release.tag
    );
    info!(%release.tag, "self-replace выполнен — процесс должен завершиться сейчас");

    // Сбросим буферы перед выходом (на всякий случай).
    let _ = std::io::Write::flush(&mut std::io::stderr());

    Ok(())
}

/// Скачивает файл по URL на диск (упрощённо: весь бинарные релиза ~10–20 МБ, не страшно целиком в памяти).
async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()?;
    let bytes = resp.bytes().await.with_context(|| format!("скачать тело ответа {url}"))?;

    let mut file = tokio::fs::File::create(dest)
        .await
        .with_context(|| format!("create {}", dest.display()))?;
    file.write_all(&bytes).await?;
    file.flush().await?;
    Ok(())
}

/// Извлекает содержимое zip-архива в каталог `to_dir`.
fn extract_zip(zip_path: &Path, to_dir: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("open {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("zip-архив повреждён: {}", zip_path.display()))?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        // Защита от Zip Slip: путь должен оставаться внутри to_dir.
        let raw_name = entry.name().replace('\\', "/");
        let clean = raw_name.trim_start_matches('/').trim_start_matches('.');
        let target = to_dir.join(clean);
        let canon_base = to_dir.canonicalize().unwrap_or_else(|_| to_dir.to_path_buf());
        if let Ok(canon_target) = target.canonicalize() {
            if !canon_target.starts_with(&canon_base) && !canon_target.as_os_str().is_empty() {
                return Err(anyhow!(
                    "Обнаружен Zip Slip: entry '{}' выходит за пределы {}",
                    entry.name(),
                    to_dir.display()
                ));
            }
        }

        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&target)
                .with_context(|| format!("create {}", target.display()))?;
            std::io::copy(&mut entry, &mut out)?;
            // Unix-права: переносим только если платформа поддерживает (не Windows).
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = entry.unix_mode() {
                    let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode));
                }
            }
        }
    }
    Ok(())
}

/// Находит бинарник ob2h (exe/elf/mach-o) в распакованном каталоге.
fn find_binary(unpack_dir: &Path) -> anyhow::Result<PathBuf> {
    let candidates: &[&str] = if cfg!(windows) {
        &["ob2h.exe", "bin/ob2h.exe"]
    } else {
        &["ob2h", "bin/ob2h", "ob2h.app/Contents/MacOS/ob2h"]
    };
    for c in candidates {
        let p = unpack_dir.join(c);
        if p.is_file() {
            return Ok(p);
        }
    }
    // Фолбэк: ищем первый файл с подходящим именем в распакованном каталоге.
    let mut found = None;
    let want = if cfg!(windows) { "ob2h.exe" } else { "ob2h" };
    walk_dir(unpack_dir, &mut |p| {
        if p.file_name().map(|n| n.to_string_lossy().eq_ignore_ascii_case(want)).unwrap_or(false) {
            found = Some(p.to_path_buf());
        }
    });
    found.ok_or_else(|| {
        anyhow!(
            "В распакованном архиве {} не найден бинарник ob2h (искали {want:?})",
            unpack_dir.display()
        )
    })
}

/// Рекурсивный обход каталога (простой, без лишних зависимостей).
fn walk_dir(dir: &Path, cb: &mut dyn FnMut(&Path)) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for entry in read.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            walk_dir(&path, cb);
        } else {
            cb(&path);
        }
    }
}

/// Запускает бинарник `exe_path` с указанными аргументами, ожидает завершения,
/// проверяет exit code 0.
fn run_subcommand(exe_path: &Path, args: &[&str], what: &str) -> anyhow::Result<()> {
    info!(exe = %exe_path.display(), ?args, "Запускаем: {what}");
    let status = std::process::Command::new(exe_path)
        .args(args)
        .status()
        .with_context(|| format!("spawn {} {}", exe_path.display(), args.join(" ")))?;
    if !status.success() {
        return Err(anyhow!(
            "Команда ob2h {} завершилась с кодом {status:?} ({what})",
            args.join(" ")
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Юнит-тесты (без сети — только парсинг JSON-фикстур и логика).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    const GH_RELEASE_JSON: &str = r###"
    {
      "tag_name": "v1.7.1",
      "name": "Release 1.7.1 — AST hotfixes",
      "body": "## Fixed\n- caller attribution\n",
      "draft": false,
      "prerelease": false,
      "published_at": "2026-10-10T12:00:00Z",
      "assets": [
        {"name": "ob2h-x86_64-pc-windows-msvc.zip", "size": 10485760,
         "browser_download_url": "https://github.com/presnyakov/ob2h/releases/download/v1.7.1/ob2h-x86_64-pc-windows-msvc.zip",
         "content_type": "application/zip"},
        {"name": "ob2h-x86_64-unknown-linux-gnu.tar.gz", "size": 9437184,
         "browser_download_url": "https://github.com/presnyakov/ob2h/releases/download/v1.7.1/ob2h-x86_64-unknown-linux-gnu.tar.gz",
         "content_type": "application/gzip"},
        {"name": "checksums.txt", "size": 512,
         "browser_download_url": "https://github.com/presnyakov/ob2h/releases/download/v1.7.1/checksums.txt",
         "content_type": "text/plain"}
      ]
    }"###;

    const GH_RELEASE_NIGHTLY_JSON: &str = r###"
    {
      "tag_name": "v1.7.2-rc.1",
      "name": "Pre-release 1.7.2-rc.1",
      "body": "nightly build",
      "draft": false,
      "prerelease": true,
      "published_at": "2026-10-11T00:00:00Z",
      "assets": [
        {"name": "ob2h-windows-amd64.zip", "size": 10485760,
         "browser_download_url": "https://github.com/presnyakov/ob2h/releases/download/v1.7.2-rc.1/ob2h-windows-amd64.zip",
         "content_type": "application/zip"}
      ]
    }"###;

    #[test]
    fn current_version_is_valid_semver() {
        let v = current_version();
        assert!(v.major > 0 || v.minor > 0 || v.patch > 0);
    }

    #[test]
    fn parse_tag_strips_v_prefix_and_compares() {
        let cur = semver::Version::parse("1.7.0").unwrap();
        assert!(is_version_newer(&cur, "v1.7.1"));
        assert!(is_version_newer(&cur, "1.8.0"));
        assert!(!is_version_newer(&cur, "v1.7.0"));
        assert!(!is_version_newer(&cur, "v1.6.99"));
        assert!(!is_version_newer(&cur, "garbage"));
    }

    #[test]
    fn pick_windows_asset_finds_msvc_zip() {
        let release: GitHubRelease = serde_json::from_str(GH_RELEASE_JSON).unwrap();
        let win = pick_windows_asset(&release.assets).expect("windows zip");
        assert!(win.name.ends_with(".zip"));
        assert!(win.name.contains("windows"));
    }

    #[test]
    fn pick_native_asset_on_windows_prefers_windows_zip() {
        let release: GitHubRelease = serde_json::from_str(GH_RELEASE_JSON).unwrap();
        // Этот тест проходит только на Windows; на других платформах будет выбран Linux/mac.
        if cfg!(windows) {
            let asset = pick_native_asset(&release.assets).expect("native asset");
            assert!(asset.name.contains("windows"), "нашли {0}", asset.name);
        }
    }

    #[test]
    fn release_info_from_gh_release() {
        let release: GitHubRelease = serde_json::from_str(GH_RELEASE_JSON).unwrap();
        let info = ReleaseInfo::from_gh_release(&release).expect("parse release");
        assert_eq!(info.version, "1.7.1");
        assert_eq!(info.tag, "v1.7.1");
        assert!(info.title.as_ref().unwrap().contains("AST hotfixes"));
        assert!(info.asset.is_some());
    }

    #[test]
    fn release_info_nightly_parses_prerelease_tag() {
        let release: GitHubRelease = serde_json::from_str(GH_RELEASE_NIGHTLY_JSON).unwrap();
        let info = ReleaseInfo::from_gh_release(&release).expect("parse nightly");
        assert_eq!(info.version, "1.7.2-rc.1");
        let cur = semver::Version::parse("1.7.1").unwrap();
        assert!(is_version_newer(&cur, &info.tag));
    }

    #[test]
    fn extract_zip_roundtrip_zip_slip_rejected() {
        // Создаём во временной папке zip-архив с одним бинарником ob2h.exe.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let zip_path = root.join("test.zip");
        let unpack_dir = root.join("out");

        // Пишем zip.
        {
            let zip_file = std::fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(zip_file);
            let opts = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
            zw.start_file("bin/ob2h.exe", opts).unwrap();
            use std::io::Write;
            zw.write_all(b"fake binary content").unwrap();

            // Zip Slip-попытка: запись за пределы каталога → должна быть отклонена при распаковке.
            zw.start_file("../../evil.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            zw.write_all(b"pwned").unwrap();
            zw.finish().unwrap();
        }

        let res = extract_zip(&zip_path, &unpack_dir);
        // Мы ожидаем ОШИБКУ (zip-slip entry → Err).
        assert!(res.is_err(), "zip-slip должен быть отклонён, получили {res:?}");
    }

    #[test]
    fn find_binary_looks_in_candidates_and_walk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        // Создаём подкаталог bin
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let exe_name = if cfg!(windows) { "ob2h.exe" } else { "ob2h" };
        std::fs::write(dir.join("bin").join(exe_name), b"x").unwrap();
        assert!(find_binary(dir).is_ok());
    }
}
