//! `mask.bin` 解析、MSB-first 位查询与 LRU 缓存。
//!
//! 产线口径（v1.1 §2.7）：
//! - 每帧 160×90，缩略图行优先；
//! - 每帧 160*90/8 = 1800 字节；
//! - `idx = y * 160 + x`
//! - `byte = frame * 1800 + idx / 8`
//! - `bit  = 7 - (idx % 8)`（MSB-first）
//!
//! 镜像播放时只翻转归一化 x：`u = 1.0 - u`。

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::Path;
use std::sync::Arc;

pub const MASK_W: usize = 160;
pub const MASK_H: usize = 90;
pub const FRAME_BYTES: usize = MASK_W * MASK_H / 8; // 1800
pub const MAX_MASK_BYTES: u64 = 2 * 1024 * 1024;
pub const CACHE_CAPACITY: usize = 16;

#[derive(Debug, Clone)]
pub struct MaskData {
    bytes: Vec<u8>,
    frames: usize,
}

/// 返回 `(byte_offset, bit_index)`；x/y 必须在 160×90 内。
pub fn bit_position(frame: u32, x: usize, y: usize) -> (usize, u8) {
    debug_assert!(x < MASK_W && y < MASK_H);
    let idx = y * MASK_W + x;
    let byte = frame as usize * FRAME_BYTES + idx / 8;
    let bit = 7 - (idx % 8) as u8;
    (byte, bit)
}

impl MaskData {
    pub fn from_bytes(bytes: Vec<u8>, expected_frames: u32) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_MASK_BYTES {
            return Err(format!(
                "mask.bin 超过 2MB（{} 字节），拒绝加载",
                bytes.len()
            ));
        }
        if bytes.is_empty() || bytes.len() % FRAME_BYTES != 0 {
            return Err(format!(
                "mask.bin 长度非法：{}（应为 {FRAME_BYTES} 的整数倍）",
                bytes.len()
            ));
        }
        let frames = bytes.len() / FRAME_BYTES;
        if expected_frames > 0 && frames < expected_frames as usize {
            return Err(format!(
                "mask.bin 帧数不足：文件 {frames} 帧 < meta.frames {expected_frames}"
            ));
        }
        Ok(Self { bytes, frames })
    }

    #[allow(dead_code)]
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// 归一化坐标命中测试。`u`/`v` 为窗口内归一化坐标（左上原点、y 向下）。
    #[allow(dead_code)]
    pub fn hit(&self, frame: u32, u: f64, v: f64, mirror: bool) -> bool {
        if self.frames == 0 {
            return false;
        }
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return false;
        }
        let u = if mirror { 1.0 - u } else { u };
        let x = ((u * MASK_W as f64).floor() as usize).min(MASK_W - 1);
        let y = ((v * MASK_H as f64).floor() as usize).min(MASK_H - 1);
        let frame = (frame as usize).min(self.frames - 1);
        let (byte, bit) = bit_position(frame as u32, x, y);
        self.bytes
            .get(byte)
            .map(|value| (value >> bit) & 1 == 1)
            .unwrap_or(false)
    }

    /// 带邻域容错（±radius 格）的命中测试：避免发丝、手臂缝隙或 100ms 帧差导致点击穿透漏点。
    pub fn hit_relaxed(&self, frame: u32, u: f64, v: f64, mirror: bool, radius: isize) -> bool {
        if self.frames == 0 {
            return false;
        }
        if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
            return false;
        }
        let u = if mirror { 1.0 - u } else { u };
        let cx = ((u * MASK_W as f64).floor() as isize).clamp(0, (MASK_W - 1) as isize);
        let cy = ((v * MASK_H as f64).floor() as isize).clamp(0, (MASK_H - 1) as isize);
        let frame = (frame as usize).min(self.frames - 1) as u32;
        let r = radius.clamp(0, 3);
        for dy in -r..=r {
            let y = cy + dy;
            if y < 0 || y >= MASK_H as isize {
                continue;
            }
            for dx in -r..=r {
                let x = cx + dx;
                if x < 0 || x >= MASK_W as isize {
                    continue;
                }
                let (byte, bit) = bit_position(frame, x as usize, y as usize);
                if self
                    .bytes
                    .get(byte)
                    .map(|value| (value >> bit) & 1 == 1)
                    .unwrap_or(false)
                {
                    return true;
                }
            }
        }
        false
    }
}

/// 校验文件大小并读取原始字节；> 2MB 在读盘前就拒绝。
pub fn load_mask_bytes(path: &Path, expected_frames: u32) -> Result<Vec<u8>, String> {
    let meta =
        fs::metadata(path).map_err(|err| format!("读取掩码失败 {}: {err}", path.display()))?;
    if meta.len() > MAX_MASK_BYTES {
        return Err(format!(
            "mask.bin 超过 2MB（{} 字节），拒绝加载: {}",
            meta.len(),
            path.display()
        ));
    }
    let bytes = fs::read(path).map_err(|err| format!("读取掩码失败 {}: {err}", path.display()))?;
    // 预校验一次，避免把坏文件塞进缓存。
    MaskData::from_bytes(bytes.clone(), expected_frames)?;
    Ok(bytes)
}

/// 16 段 LRU 缓存（契约要求上限 16 段）。
pub struct MaskCache {
    capacity: usize,
    map: HashMap<String, Arc<MaskData>>,
    order: VecDeque<String>,
}

impl Default for MaskCache {
    fn default() -> Self {
        Self::new(CACHE_CAPACITY)
    }
}

impl MaskCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[allow(dead_code)]
    pub fn contains(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
    }

    pub fn get_or_load<F>(
        &mut self,
        key: &str,
        expected_frames: u32,
        loader: F,
    ) -> Result<Arc<MaskData>, String>
    where
        F: FnOnce() -> Result<Vec<u8>, String>,
    {
        if let Some(hit) = self.map.get(key).cloned() {
            self.touch(key);
            return Ok(hit);
        }
        let bytes = loader()?;
        let parsed = Arc::new(MaskData::from_bytes(bytes, expected_frames)?);
        self.map.insert(key.to_string(), parsed.clone());
        self.order.push_back(key.to_string());
        while self.map.len() > self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.map.remove(&evicted);
            } else {
                break;
            }
        }
        Ok(parsed)
    }

    fn touch(&mut self, key: &str) {
        if let Some(index) = self.order.iter().position(|item| item == key) {
            self.order.remove(index);
        }
        self.order.push_back(key.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_frame_mask() -> MaskData {
        MaskData::from_bytes(vec![0u8; FRAME_BYTES], 1).expect("synthetic mask")
    }

    fn pixel_center(x: usize, y: usize) -> (f64, f64) {
        (
            (x as f64 + 0.5) / MASK_W as f64,
            (y as f64 + 0.5) / MASK_H as f64,
        )
    }

    #[test]
    fn msb_first_bit_layout_is_exact() {
        let mut mask = one_frame_mask();
        let (byte, bit) = bit_position(0, 5, 3);
        // idx = 3*160 + 5 = 485；byte = 60；bit = 7 - (485 % 8) = 2。
        assert_eq!(byte, 60);
        assert_eq!(bit, 2);
        mask.bytes[byte] |= 1 << bit;

        let (u, v) = pixel_center(5, 3);
        assert!(mask.hit(0, u, v, false));
        let (other_u, other_v) = pixel_center(6, 3);
        assert!(!mask.hit(0, other_u, other_v, false));
    }

    #[test]
    fn mirror_flips_x_axis() {
        let mut mask = one_frame_mask();
        let (byte, bit) = bit_position(0, 5, 3);
        mask.bytes[byte] |= 1 << bit;

        let (u, v) = pixel_center(5, 3);
        let mirrored_u = 1.0 - u;
        assert!(mask.hit(0, mirrored_u, v, true), "镜像后应命中同一像素");
        assert!(!mask.hit(0, u, v, true), "镜像后原 u 应落到另一侧");
        assert!(mask.hit(0, u, v, false), "非镜像原 u 仍命中");
    }

    #[test]
    fn frame_offset_uses_meta_frame_count() {
        let mut bytes = vec![0u8; FRAME_BYTES * 3];
        let (byte, bit) = bit_position(2, 159, 89);
        bytes[byte] |= 1 << bit;
        let mask = MaskData::from_bytes(bytes, 3).expect("3-frame mask");
        assert_eq!(mask.frames(), 3);
        let (u, v) = pixel_center(159, 89);
        assert!(mask.hit(2, u, v, false));
        assert!(!mask.hit(1, u, v, false));
        // 越界帧夹到最后一帧，不 panic。
        assert!(mask.hit(99, u, v, false));
    }

    #[test]
    fn rejects_oversize_and_short_masks() {
        let oversize = vec![0u8; MAX_MASK_BYTES as usize + FRAME_BYTES];
        assert!(MaskData::from_bytes(oversize, 1).is_err());
        // meta 写 300 帧但文件只有 1 帧 → 拒绝。
        assert!(MaskData::from_bytes(vec![0u8; FRAME_BYTES], 300).is_err());
    }

    #[test]
    fn loads_real_mask_and_uses_meta_frames() {
        let clips = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("assets")
            .join("chars")
            .join("claude")
            .join("clips");
        if !clips.is_dir() {
            return;
        }
        let entry = match std::fs::read_dir(&clips)
            .expect("read clips")
            .flatten()
            .find(|item| item.file_name().to_string_lossy().ends_with(".mask.bin"))
        {
            Some(entry) => entry,
            None => return,
        };
        let mask_path = entry.path();
        let stem = entry.file_name().to_string_lossy().replace(".mask.bin", "");
        let meta_path = clips.join(format!("{stem}.meta.json"));
        let meta: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(meta_path).expect("meta"))
                .expect("parse meta");
        let frames = meta
            .get("frames")
            .and_then(|value| value.as_u64())
            .unwrap_or(0) as u32;
        assert!(frames > 0);
        let bytes = load_mask_bytes(&mask_path, frames).expect("load real mask");
        assert_eq!(bytes.len(), frames as usize * FRAME_BYTES);
        let mask = MaskData::from_bytes(bytes, frames).expect("parse real mask");
        assert_eq!(mask.frames(), frames as usize);
    }

    #[test]
    fn lru_cache_evicts_oldest_beyond_capacity() {
        let mut cache = MaskCache::new(2);
        for key in ["a", "b", "c"] {
            cache
                .get_or_load(key, 1, || Ok(vec![0u8; FRAME_BYTES]))
                .expect("load");
        }
        assert_eq!(cache.len(), 2);
        assert!(!cache.contains("a"));
        assert!(cache.contains("b"));
        assert!(cache.contains("c"));
    }

    #[test]
    fn hit_relaxed_covers_one_cell_margin_without_leaking_far() {
        let mut mask = one_frame_mask();
        let (byte, bit) = bit_position(0, 10, 10);
        mask.bytes[byte] |= 1 << bit;

        let (adj_u, adj_v) = pixel_center(11, 11);
        assert!(!mask.hit(0, adj_u, adj_v, false));
        assert!(mask.hit_relaxed(0, adj_u, adj_v, false, 1));

        let (far_u, far_v) = pixel_center(13, 10);
        assert!(!mask.hit_relaxed(0, far_u, far_v, false, 1));
    }
}
