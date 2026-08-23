use std::ffi::CString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rust_i18n::t;
use windows_sys::Win32::System::WindowsProgramming::{GetPrivateProfileStringA, WritePrivateProfileStringA};

const PROFILE_PATH: &[u8] = b".\\Morrowind.ini\0";

/// The Morrowind.ini preferences the GUI edits. These are the game's own
/// settings, not MGE's: nothing here appears in `mgeXE.toml`, and the two files
/// are saved independently so a broken one cannot block the other.
#[derive(Clone, Debug)]
pub struct IniSettings {
    pub fps_limit: u32,
    pub screenshots: bool,
    pub thread_loading: bool,
    pub yes_to_all: bool,
    pub high_detail_shadows: bool,
    pub show_fps: bool,
    pub disable_audio: bool,
    pub subtitles: bool,
    pub hit_fader: bool,
    pub light_constant: f32,
    pub light_linear: f32,
    pub light_quadratic: f32,
}

impl Default for IniSettings {
    fn default() -> Self {
        Self {
            fps_limit: 240,
            screenshots: false,
            thread_loading: true,
            yes_to_all: false,
            high_detail_shadows: false,
            show_fps: false,
            disable_audio: false,
            subtitles: false,
            hit_fader: true,
            light_constant: 0.0,
            light_linear: 3.0,
            light_quadratic: 0.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MorrowindProfile {
    absolute_path: PathBuf,
}

impl MorrowindProfile {
    pub fn open(root: &Path) -> Result<Self> {
        let absolute_path = root.join("Morrowind.ini");
        if !absolute_path.is_file() {
            bail!(
                "{}",
                t!(
                    "messages.morrowind_ini_missing_owned",
                    path = absolute_path.display().to_string()
                )
            );
        }
        Ok(Self { absolute_path })
    }

    pub fn path(&self) -> &Path {
        &self.absolute_path
    }

    pub fn load(&self, settings: &mut IniSettings) -> Result<()> {
        if !self.absolute_path.is_file() {
            bail!(
                "{}",
                t!("messages.file_missing", path = self.absolute_path.display().to_string())
            );
        }
        settings.fps_limit = self.get_i32("General", "Max FPS", 240)?.clamp(1, 1000) as u32;
        settings.screenshots = self.get_bool01("General", "Screen Shot Enable", settings.screenshots)?;
        settings.thread_loading = !self.get_bool01("General", "DontThreadLoad", !settings.thread_loading)?;
        settings.yes_to_all = self.get_bool01("General", "AllowYesToAll", settings.yes_to_all)?;
        settings.high_detail_shadows = self.get_bool01("General", "High Detail Shadows", settings.high_detail_shadows)?;
        settings.show_fps = self.get_bool01("General", "Show FPS", settings.show_fps)?;
        settings.disable_audio = self.get_bool01("General", "Disable Audio", settings.disable_audio)?;
        settings.subtitles = self.get_bool01("General", "Subtitles", settings.subtitles)?;
        settings.hit_fader = self.get_bool01("General", "ShowHitFader", settings.hit_fader)?;

        if self.get_bool01("LightAttenuation", "UseConstant", false)? {
            settings.light_constant = self.get_f32("LightAttenuation", "ConstantValue", settings.light_constant)?;
        }
        if self.get_bool01("LightAttenuation", "UseLinear", true)? {
            settings.light_linear = self.get_f32("LightAttenuation", "LinearValue", settings.light_linear)?;
        }
        if self.get_bool01("LightAttenuation", "UseQuadratic", false)? {
            settings.light_quadratic = self.get_f32("LightAttenuation", "QuadraticValue", settings.light_quadratic)?;
        }
        Ok(())
    }

    pub fn save(&self, settings: &IniSettings) -> Result<()> {
        if !self.absolute_path.is_file() {
            bail!("{} is missing", self.absolute_path.display());
        }
        self.set_i32("General", "Max FPS", settings.fps_limit.max(1) as i32)?;
        self.set_bool01("General", "Screen Shot Enable", settings.screenshots)?;
        self.set_bool01("General", "DontThreadLoad", !settings.thread_loading)?;
        self.set_bool01("General", "AllowYesToAll", settings.yes_to_all)?;
        self.set_bool01("General", "High Detail Shadows", settings.high_detail_shadows)?;
        self.set_bool01("General", "Show FPS", settings.show_fps)?;
        self.set_bool01("General", "Disable Audio", settings.disable_audio)?;
        self.set_bool01("General", "Subtitles", settings.subtitles)?;
        self.set_bool01("General", "ShowHitFader", settings.hit_fader)?;

        // Preserve the GUI's established behavior: saving enables all three terms.
        self.set_bool01("LightAttenuation", "UseConstant", true)?;
        self.set_f32("LightAttenuation", "ConstantValue", settings.light_constant)?;
        self.set_bool01("LightAttenuation", "UseLinear", true)?;
        self.set_f32("LightAttenuation", "LinearValue", settings.light_linear)?;
        self.set_bool01("LightAttenuation", "UseQuadratic", true)?;
        self.set_f32("LightAttenuation", "QuadraticValue", settings.light_quadratic)?;
        self.flush()
    }

    pub fn get_i32(&self, section: &str, key: &str, default: i32) -> Result<i32> {
        let value = self.read(section, key, &default.to_string())?;
        Ok(value.trim().parse().unwrap_or(default))
    }

    pub fn get_bool01(&self, section: &str, key: &str, default: bool) -> Result<bool> {
        Ok(self.get_i32(section, key, i32::from(default))? != 0)
    }

    pub fn get_f32(&self, section: &str, key: &str, default: f32) -> Result<f32> {
        let value = self.read(section, key, &default.to_string())?;
        Ok(value.trim().parse().unwrap_or(default))
    }

    pub fn set_i32(&self, section: &str, key: &str, value: i32) -> Result<()> {
        self.write(section, key, &value.to_string())
    }

    pub fn set_bool01(&self, section: &str, key: &str, value: bool) -> Result<()> {
        self.set_i32(section, key, i32::from(value))
    }

    pub fn set_f32(&self, section: &str, key: &str, value: f32) -> Result<()> {
        self.write(section, key, &format!("{value:.3}"))
    }

    fn read(&self, section: &str, key: &str, default: &str) -> Result<String> {
        let section = CString::new(section)?;
        let key = CString::new(key)?;
        let default = CString::new(default)?;
        let mut buffer = [0_u8; 512];
        let length = unsafe {
            GetPrivateProfileStringA(
                section.as_ptr().cast(),
                key.as_ptr().cast(),
                default.as_ptr().cast(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                PROFILE_PATH.as_ptr(),
            )
        } as usize;
        if length >= buffer.len() - 1 {
            bail!(
                "read [{section:?}] {key:?} from {} exceeded the scalar buffer",
                self.path().display()
            );
        }
        Ok(String::from_utf8_lossy(&buffer[..length]).into_owned())
    }

    fn write(&self, section: &str, key: &str, value: &str) -> Result<()> {
        let section = CString::new(section)?;
        let key = CString::new(key)?;
        let value = CString::new(value)?;
        let success = unsafe {
            WritePrivateProfileStringA(
                section.as_ptr().cast(),
                key.as_ptr().cast(),
                value.as_ptr().cast(),
                PROFILE_PATH.as_ptr(),
            )
        };
        if success == 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("write [{section:?}] {key:?} in {}", self.path().display()));
        }
        Ok(())
    }

    fn flush(&self) -> Result<()> {
        // The documented flush form intentionally returns zero whether the
        // cache flushes or the call fails, so there is no success bit to test.
        unsafe { WritePrivateProfileStringA(std::ptr::null(), std::ptr::null(), std::ptr::null(), PROFILE_PATH.as_ptr()) };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn missing_profile_is_rejected_without_creation() {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("mge-morrowind-profile-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("Morrowind.ini");

        assert!(MorrowindProfile::open(&root).is_err());
        assert!(!path.exists());
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn native_profile_round_trip_uses_the_relative_game_path() {
        let root = test_root("配置");
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("Morrowind.ini"),
            "[General]\nMax FPS=0\nDontThreadLoad=1\nSubtitles=0\nMalformed=oops\n\n\
             [LightAttenuation]\nUseConstant=0\nConstantValue=9.0\nUseLinear=1\nLinearValue=2.5\n\
             UseQuadratic=0\nQuadraticValue=7.0\n\n[Unrelated]\nKeep=Yes\n",
        )
        .unwrap();

        let output = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("morrowind_profile::tests::profile_round_trip_child")
            .arg("--nocapture")
            .env("MGE_XE_TEST_PROFILE_ROOT", &root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let written = fs::read_to_string(root.join("Morrowind.ini")).unwrap();
        assert!(written.contains("[Unrelated]"));
        assert!(written.contains("Keep=Yes"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn profile_round_trip_child() {
        let Some(root) = std::env::var_os("MGE_XE_TEST_PROFILE_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        std::env::set_current_dir(&root).unwrap();
        let profile = MorrowindProfile::open(&root).unwrap();

        // The profile API owns case-insensitive lookup and numeric fallback.
        assert_eq!(profile.get_i32("general", "max fps", 240).unwrap(), 0);
        assert_eq!(profile.get_i32("General", "Malformed", 17).unwrap(), 17);

        let mut settings = IniSettings::default();
        profile.load(&mut settings).unwrap();
        assert_eq!(settings.fps_limit, 1);
        assert!(!settings.thread_loading);
        assert_eq!(settings.light_constant, 0.0);
        assert_eq!(settings.light_linear, 2.5);
        assert_eq!(settings.light_quadratic, 0.0);

        settings.thread_loading = true;
        settings.subtitles = true;
        settings.fps_limit = 0;
        profile.save(&settings).unwrap();
        assert_eq!(profile.get_i32("General", "Max FPS", 240).unwrap(), 1);
        assert!(!profile.get_bool01("General", "DontThreadLoad", true).unwrap());
        assert!(profile.get_bool01("General", "Subtitles", false).unwrap());
        // Preserve the approved write-all-enabled attenuation behavior.
        assert!(profile.get_bool01("LightAttenuation", "UseConstant", false).unwrap());
        assert!(profile.get_bool01("LightAttenuation", "UseLinear", false).unwrap());
        assert!(profile.get_bool01("LightAttenuation", "UseQuadratic", false).unwrap());
    }

    fn test_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("mge-morrowind-profile-{label}-{}-{nonce}", std::process::id()))
    }
}
