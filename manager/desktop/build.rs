use ico::{IconDir, IconDirEntry, IconImage, ResourceType};
use std::env;
use std::fs::File;
use std::path::PathBuf;

fn main() {
    // tauri-build requires an ICO for the Windows resource compiler. Generate a small,
    // deterministic OJOS lightning icon into OUT_DIR so source distributions do not need to
    // carry a platform-specific binary asset.
    let icon_path = generated_icons();
    let windows = tauri_build::WindowsAttributes::new().window_icon_path(icon_path);
    let attributes = tauri_build::Attributes::new().windows_attributes(windows);
    tauri_build::try_build(attributes).expect("failed to build Tauri desktop resources");
}

fn generated_icons() -> PathBuf {
    const SIZE: u32 = 64;
    const BOLT: [(f32, f32); 6] = [
        (36.0, 6.0),
        (16.0, 35.0),
        (29.0, 35.0),
        (24.0, 59.0),
        (49.0, 27.0),
        (35.0, 27.0),
    ];
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 + 0.5 - SIZE as f32 / 2.0;
            let dy = y as f32 + 0.5 - SIZE as f32 / 2.0;
            let inside_disc = dx * dx + dy * dy <= 30.0 * 30.0;
            let inside_bolt = point_in_polygon(x as f32 + 0.5, y as f32 + 0.5, &BOLT);
            let pixel = if inside_bolt {
                [244, 240, 255, 255]
            } else if inside_disc {
                [126, 20, 255, 255]
            } else {
                [0, 0, 0, 0]
            };
            rgba.extend_from_slice(&pixel);
        }
    }
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let png_path = manifest_dir.join("icons").join("icon.png");
    std::fs::create_dir_all(png_path.parent().expect("icon parent"))
        .expect("create icon directory");
    let png_file = File::create(&png_path).expect("create generated PNG icon");
    let mut encoder = png::Encoder::new(png_file, SIZE, SIZE);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("write PNG icon header");
    writer.write_image_data(&rgba).expect("write PNG icon");
    drop(writer);

    let image = IconImage::from_rgba_data(SIZE, SIZE, rgba);
    let mut directory = IconDir::new(ResourceType::Icon);
    directory.add_entry(IconDirEntry::encode(&image).expect("encode generated icon"));
    let path = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("ojos.ico");
    let file = File::create(&path).expect("create generated icon");
    directory.write(file).expect("write generated icon");
    path
}

fn point_in_polygon(x: f32, y: f32, polygon: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut previous = polygon.len() - 1;
    for current in 0..polygon.len() {
        let (x1, y1) = polygon[current];
        let (x2, y2) = polygon[previous];
        if (y1 > y) != (y2 > y) && x < (x2 - x1) * (y - y1) / (y2 - y1) + x1 {
            inside = !inside;
        }
        previous = current;
    }
    inside
}
