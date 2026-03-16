use zed_extension_api as zed;

struct CrystalExtension;

impl zed::Extension for CrystalExtension {
    fn new() -> Self {
        Self
    }
}

zed::register_extension!(CrystalExtension);
