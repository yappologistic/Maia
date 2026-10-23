//! Colors, fonts and vector icons.

use egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2, pos2, vec2};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ThemeName {
    #[default]
    Midnight,
    Graphite,
    Lagoon,
    Navy,
    #[serde(alias = "Light")]
    Paper,
    Linen,
    /// Well-loved editor themes, in their published colours.
    Mocha,
    TokyoNight,
    Gruvbox,
    Latte,
    Dracula,
    Nord,
    OneDark,
    SolarizedLight,
    RosePineDawn,
    Everforest,
    Kanagawa,
    GruvboxLight,
    AyuLight,
    RosePine,
    SolarizedDark,
    NightOwl,
    EverforestLight,
    TokyoNightDay,
    KanagawaLotus,
    CatppuccinFrappe,
    CatppuccinMacchiato,
    Monokai,
    Palenight,
    AyuDark,
    AyuMirage,
    RosePineMoon,
    TokyoNightStorm,
    KanagawaDragon,
    Nightfox,
    FlexokiDark,
    OneLight,
    LightOwl,
    Dayfox,
    FlexokiLight,
    GitHubDark,
    GitHubDimmed,
    Zenburn,
    TomorrowNight,
    OceanicNext,
    Horizon,
    Cobalt2,
    Synthwave,
    Moonlight,
    Poimandres,
    Oxocarbon,
    ModusVivendi,
    Iceberg,
    GitHubLight,
    Tomorrow,
    IcebergLight,
    ModusOperandi,
    SelenizedLight,
    QuietLight,
    PaperColor,
    DarkModern,
    Darcula,
    XcodeDark,
    MaterialDarker,
    DoomOne,
    Sonokai,
    GruvboxMaterial,
    Vesper,
    Andromeda,
    ShadesOfPurple,
    Carbonfox,
    Duskfox,
    VitesseDark,
    LightModern,
    IntelliJLight,
    XcodeLight,
    Dawnfox,
    VitesseLight,
    GruvboxMaterialLight,
    Alabaster,
    MonokaiPro,
    Everblush,
    Jellybeans,
    Srcery,
    Terafox,
    EdgeDark,
    Eighties,
    Panda,
    Cyberdream,
    Mariana,
    Embark,
    SpacemacsDark,
    MelangeDark,
    EdgeLight,
    MonokaiProLight,
    Alucard,
    Zenbones,
    SpacemacsLight,
    Leuven,
    DoomOneLight,
}

impl ThemeName {
    pub const ALL: [ThemeName; 100] = [
        ThemeName::Midnight,
        ThemeName::Graphite,
        ThemeName::Lagoon,
        ThemeName::Navy,
        ThemeName::Mocha,
        ThemeName::TokyoNight,
        ThemeName::Gruvbox,
        ThemeName::Dracula,
        ThemeName::Nord,
        ThemeName::OneDark,
        ThemeName::Everforest,
        ThemeName::Kanagawa,
        ThemeName::RosePine,
        ThemeName::SolarizedDark,
        ThemeName::NightOwl,
        ThemeName::CatppuccinFrappe,
        ThemeName::CatppuccinMacchiato,
        ThemeName::Monokai,
        ThemeName::Palenight,
        ThemeName::AyuDark,
        ThemeName::AyuMirage,
        ThemeName::RosePineMoon,
        ThemeName::TokyoNightStorm,
        ThemeName::KanagawaDragon,
        ThemeName::Nightfox,
        ThemeName::FlexokiDark,
        ThemeName::GitHubDark,
        ThemeName::GitHubDimmed,
        ThemeName::Zenburn,
        ThemeName::TomorrowNight,
        ThemeName::OceanicNext,
        ThemeName::Horizon,
        ThemeName::Cobalt2,
        ThemeName::Synthwave,
        ThemeName::Moonlight,
        ThemeName::Poimandres,
        ThemeName::Oxocarbon,
        ThemeName::ModusVivendi,
        ThemeName::Iceberg,
        ThemeName::DarkModern,
        ThemeName::Darcula,
        ThemeName::XcodeDark,
        ThemeName::MaterialDarker,
        ThemeName::DoomOne,
        ThemeName::Sonokai,
        ThemeName::GruvboxMaterial,
        ThemeName::Vesper,
        ThemeName::Andromeda,
        ThemeName::ShadesOfPurple,
        ThemeName::Carbonfox,
        ThemeName::Duskfox,
        ThemeName::VitesseDark,
        ThemeName::Paper,
        ThemeName::Linen,
        ThemeName::Latte,
        ThemeName::SolarizedLight,
        ThemeName::RosePineDawn,
        ThemeName::GruvboxLight,
        ThemeName::AyuLight,
        ThemeName::EverforestLight,
        ThemeName::TokyoNightDay,
        ThemeName::KanagawaLotus,
        ThemeName::OneLight,
        ThemeName::LightOwl,
        ThemeName::Dayfox,
        ThemeName::FlexokiLight,
        ThemeName::GitHubLight,
        ThemeName::Tomorrow,
        ThemeName::IcebergLight,
        ThemeName::ModusOperandi,
        ThemeName::SelenizedLight,
        ThemeName::QuietLight,
        ThemeName::PaperColor,
        ThemeName::LightModern,
        ThemeName::IntelliJLight,
        ThemeName::XcodeLight,
        ThemeName::Dawnfox,
        ThemeName::VitesseLight,
        ThemeName::GruvboxMaterialLight,
        ThemeName::Alabaster,
        ThemeName::MonokaiPro,
        ThemeName::Everblush,
        ThemeName::Jellybeans,
        ThemeName::Srcery,
        ThemeName::Terafox,
        ThemeName::EdgeDark,
        ThemeName::Eighties,
        ThemeName::Panda,
        ThemeName::Cyberdream,
        ThemeName::Mariana,
        ThemeName::Embark,
        ThemeName::SpacemacsDark,
        ThemeName::MelangeDark,
        ThemeName::EdgeLight,
        ThemeName::MonokaiProLight,
        ThemeName::Alucard,
        ThemeName::Zenbones,
        ThemeName::SpacemacsLight,
        ThemeName::Leuven,
        ThemeName::DoomOneLight,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ThemeName::Midnight => "Midnight",
            ThemeName::Graphite => "Graphite",
            ThemeName::Lagoon => "Lagoon",
            ThemeName::Navy => "Navy",
            ThemeName::Paper => "Paper",
            ThemeName::Linen => "Linen",
            ThemeName::Mocha => "Catppuccin Mocha",
            ThemeName::TokyoNight => "Tokyo Night",
            ThemeName::Gruvbox => "Gruvbox",
            ThemeName::Latte => "Catppuccin Latte",
            ThemeName::Dracula => "Dracula",
            ThemeName::Nord => "Nord",
            ThemeName::OneDark => "One Dark",
            ThemeName::SolarizedLight => "Solarized Light",
            ThemeName::RosePineDawn => "Rosé Pine Dawn",
            ThemeName::Everforest => "Everforest",
            ThemeName::Kanagawa => "Kanagawa",
            ThemeName::GruvboxLight => "Gruvbox Light",
            ThemeName::AyuLight => "Ayu Light",
            ThemeName::RosePine => "Rosé Pine",
            ThemeName::SolarizedDark => "Solarized Dark",
            ThemeName::NightOwl => "Night Owl",
            ThemeName::EverforestLight => "Everforest Light",
            ThemeName::TokyoNightDay => "Tokyo Night Day",
            ThemeName::KanagawaLotus => "Kanagawa Lotus",
            ThemeName::CatppuccinFrappe => "Catppuccin Frappé",
            ThemeName::CatppuccinMacchiato => "Catppuccin Macchiato",
            ThemeName::Monokai => "Monokai",
            ThemeName::Palenight => "Material Palenight",
            ThemeName::AyuDark => "Ayu Dark",
            ThemeName::AyuMirage => "Ayu Mirage",
            ThemeName::RosePineMoon => "Rosé Pine Moon",
            ThemeName::TokyoNightStorm => "Tokyo Night Storm",
            ThemeName::KanagawaDragon => "Kanagawa Dragon",
            ThemeName::Nightfox => "Nightfox",
            ThemeName::FlexokiDark => "Flexoki Dark",
            ThemeName::OneLight => "One Light",
            ThemeName::LightOwl => "Light Owl",
            ThemeName::Dayfox => "Dayfox",
            ThemeName::FlexokiLight => "Flexoki Light",
            ThemeName::GitHubDark => "GitHub Dark",
            ThemeName::GitHubDimmed => "GitHub Dark Dimmed",
            ThemeName::Zenburn => "Zenburn",
            ThemeName::TomorrowNight => "Tomorrow Night",
            ThemeName::OceanicNext => "Oceanic Next",
            ThemeName::Horizon => "Horizon",
            ThemeName::Cobalt2 => "Cobalt2",
            ThemeName::Synthwave => "Synthwave '84",
            ThemeName::Moonlight => "Moonlight",
            ThemeName::Poimandres => "Poimandres",
            ThemeName::Oxocarbon => "Oxocarbon",
            ThemeName::ModusVivendi => "Modus Vivendi",
            ThemeName::Iceberg => "Iceberg",
            ThemeName::GitHubLight => "GitHub Light",
            ThemeName::Tomorrow => "Tomorrow",
            ThemeName::IcebergLight => "Iceberg Light",
            ThemeName::ModusOperandi => "Modus Operandi",
            ThemeName::SelenizedLight => "Selenized Light",
            ThemeName::QuietLight => "Quiet Light",
            ThemeName::PaperColor => "PaperColor",
            ThemeName::DarkModern => "Dark Modern",
            ThemeName::Darcula => "Darcula",
            ThemeName::XcodeDark => "Xcode Dark",
            ThemeName::MaterialDarker => "Material Darker",
            ThemeName::DoomOne => "Doom One",
            ThemeName::Sonokai => "Sonokai",
            ThemeName::GruvboxMaterial => "Gruvbox Material",
            ThemeName::Vesper => "Vesper",
            ThemeName::Andromeda => "Andromeda",
            ThemeName::ShadesOfPurple => "Shades of Purple",
            ThemeName::Carbonfox => "Carbonfox",
            ThemeName::Duskfox => "Duskfox",
            ThemeName::VitesseDark => "Vitesse Dark",
            ThemeName::LightModern => "Light Modern",
            ThemeName::IntelliJLight => "IntelliJ Light",
            ThemeName::XcodeLight => "Xcode Light",
            ThemeName::Dawnfox => "Dawnfox",
            ThemeName::VitesseLight => "Vitesse Light",
            ThemeName::GruvboxMaterialLight => "Gruvbox Material Light",
            ThemeName::Alabaster => "Alabaster",
            ThemeName::MonokaiPro => "Monokai Pro",
            ThemeName::Everblush => "Everblush",
            ThemeName::Jellybeans => "Jellybeans",
            ThemeName::Srcery => "Srcery",
            ThemeName::Terafox => "Terafox",
            ThemeName::EdgeDark => "Edge Dark",
            ThemeName::Eighties => "Eighties",
            ThemeName::Panda => "Panda",
            ThemeName::Cyberdream => "Cyberdream",
            ThemeName::Mariana => "Mariana",
            ThemeName::Embark => "Embark",
            ThemeName::SpacemacsDark => "Spacemacs Dark",
            ThemeName::MelangeDark => "Melange Dark",
            ThemeName::EdgeLight => "Edge Light",
            ThemeName::MonokaiProLight => "Monokai Pro Light",
            ThemeName::Alucard => "Alucard",
            ThemeName::Zenbones => "Zenbones",
            ThemeName::SpacemacsLight => "Spacemacs Light",
            ThemeName::Leuven => "Leuven",
            ThemeName::DoomOneLight => "Doom One Light",
        }
    }
    /// A line about the theme, under its name in the theme picker.
    pub fn blurb(self) -> &'static str {
        match self {
            ThemeName::Midnight => "Graphite with teal",
            ThemeName::Graphite => "Grey with amber",
            ThemeName::Lagoon => "Deep teal",
            ThemeName::Navy => "Dark blue",
            ThemeName::Paper => "Clean white",
            ThemeName::Linen => "Warm off-white",
            ThemeName::Mocha => "Pastels on dark",
            ThemeName::TokyoNight => "City lights at night",
            ThemeName::Gruvbox => "Warm and retro",
            ThemeName::Latte => "Pastels on light",
            ThemeName::Dracula => "Purple and pink",
            ThemeName::Nord => "Arctic blue-grey",
            ThemeName::OneDark => "The classic editor dark",
            ThemeName::SolarizedLight => "Warm and low-glare",
            ThemeName::RosePineDawn => "Soft rose and pine",
            ThemeName::Everforest => "Calm forest greens",
            ThemeName::Kanagawa => "Ink and a great wave",
            ThemeName::GruvboxLight => "Warm and retro, by day",
            ThemeName::AyuLight => "Bright and clean",
            ThemeName::RosePine => "Muted rose and iris",
            ThemeName::SolarizedDark => "Calm teal depths",
            ThemeName::NightOwl => "Deep blue for late hours",
            ThemeName::EverforestLight => "Soft greens on cream",
            ThemeName::TokyoNightDay => "Cool blues by day",
            ThemeName::KanagawaLotus => "Ink on parchment",
            ThemeName::CatppuccinFrappe => "Pastels on slate",
            ThemeName::CatppuccinMacchiato => "Pastels on deep slate",
            ThemeName::Monokai => "The vivid classic",
            ThemeName::Palenight => "Soft purple night",
            ThemeName::AyuDark => "Deep and sharp",
            ThemeName::AyuMirage => "Muted blue dusk",
            ThemeName::RosePineMoon => "Rose and iris at dusk",
            ThemeName::TokyoNightStorm => "Stormy city blues",
            ThemeName::KanagawaDragon => "Ink, warm and dark",
            ThemeName::Nightfox => "Cool and calm",
            ThemeName::FlexokiDark => "Ink on dark paper",
            ThemeName::OneLight => "The classic editor light",
            ThemeName::LightOwl => "Gentle and bright",
            ThemeName::Dayfox => "Warm morning light",
            ThemeName::FlexokiLight => "Ink on paper",
            ThemeName::GitHubDark => "GitHub's own dark",
            ThemeName::GitHubDimmed => "GitHub's softer dark",
            ThemeName::Zenburn => "Low contrast, easy on the eyes",
            ThemeName::TomorrowNight => "Muted pastels at night",
            ThemeName::OceanicNext => "Deep sea blues",
            ThemeName::Horizon => "Warm pinks at dusk",
            ThemeName::Cobalt2 => "Bold blue and gold",
            ThemeName::Synthwave => "Neon from the eighties",
            ThemeName::Moonlight => "Soft blues by moonlight",
            ThemeName::Poimandres => "Cool mint on slate",
            ThemeName::Oxocarbon => "Carbon grey, sharp colour",
            ThemeName::ModusVivendi => "Highest contrast, dark",
            ThemeName::Iceberg => "Cool blues on ink",
            ThemeName::GitHubLight => "GitHub's own light",
            ThemeName::Tomorrow => "Muted pastels by day",
            ThemeName::IcebergLight => "Cool blues on frost",
            ThemeName::ModusOperandi => "Highest contrast, light",
            ThemeName::SelenizedLight => "Warm paper, clear colours",
            ThemeName::QuietLight => "Quiet and soft",
            ThemeName::PaperColor => "Crisp as printed paper",
            ThemeName::DarkModern => "VS Code's own dark",
            ThemeName::Darcula => "JetBrains' classic dark",
            ThemeName::XcodeDark => "Apple's editor at night",
            ThemeName::MaterialDarker => "Material colours on charcoal",
            ThemeName::DoomOne => "One Dark, by way of Emacs",
            ThemeName::Sonokai => "Monokai, softened",
            ThemeName::GruvboxMaterial => "Gruvbox, softer and earthier",
            ThemeName::Vesper => "Peach and mint on black",
            ThemeName::Andromeda => "Bright colours in deep space",
            ThemeName::ShadesOfPurple => "Purple and gold",
            ThemeName::Carbonfox => "Nightfox on carbon black",
            ThemeName::Duskfox => "Purple twilight",
            ThemeName::VitesseDark => "Quiet and muted, dark",
            ThemeName::LightModern => "VS Code's own light",
            ThemeName::IntelliJLight => "JetBrains' own light",
            ThemeName::XcodeLight => "Apple's editor by day",
            ThemeName::Dawnfox => "Rosy morning light",
            ThemeName::VitesseLight => "Quiet and muted, light",
            ThemeName::GruvboxMaterialLight => "Gruvbox on soft cream",
            ThemeName::Alabaster => "Colour only where it counts",
            ThemeName::MonokaiPro => "Monokai, softened and warm",
            ThemeName::Everblush => "Soft pastels on near-black",
            ThemeName::Jellybeans => "Muted colours on black",
            ThemeName::Srcery => "Bold and earthy on dark brown",
            ThemeName::Terafox => "Deep sea green",
            ThemeName::EdgeDark => "Calm and clear, dark",
            ThemeName::Eighties => "Pastels on charcoal",
            ThemeName::Panda => "Pink and teal on grey",
            ThemeName::Cyberdream => "Neon on near-black",
            ThemeName::Mariana => "Slate blue with warm accents",
            ThemeName::Embark => "Deep space purple",
            ThemeName::SpacemacsDark => "Purple and blue on grey",
            ThemeName::MelangeDark => "Warm cocoa",
            ThemeName::EdgeLight => "Calm and clear, light",
            ThemeName::MonokaiProLight => "Monokai's colours on warm white",
            ThemeName::Alucard => "Dracula's light side",
            ThemeName::Zenbones => "Ink on paper, colour sparingly",
            ThemeName::SpacemacsLight => "Purple ink on parchment",
            ThemeName::Leuven => "Crisp blues on white",
            ThemeName::DoomOneLight => "One's colours, bright",
        }
    }
    pub fn is_light(self) -> bool {
        matches!(self, ThemeName::Paper | ThemeName::Linen | ThemeName::Latte | ThemeName::SolarizedLight | ThemeName::RosePineDawn | ThemeName::GruvboxLight | ThemeName::AyuLight | ThemeName::EverforestLight | ThemeName::TokyoNightDay | ThemeName::KanagawaLotus | ThemeName::OneLight | ThemeName::LightOwl | ThemeName::Dayfox | ThemeName::FlexokiLight | ThemeName::GitHubLight | ThemeName::Tomorrow | ThemeName::IcebergLight | ThemeName::ModusOperandi | ThemeName::SelenizedLight | ThemeName::QuietLight | ThemeName::PaperColor | ThemeName::LightModern | ThemeName::IntelliJLight | ThemeName::XcodeLight | ThemeName::Dawnfox | ThemeName::VitesseLight | ThemeName::GruvboxMaterialLight | ThemeName::Alabaster | ThemeName::EdgeLight | ThemeName::MonokaiProLight | ThemeName::Alucard | ThemeName::Zenbones | ThemeName::SpacemacsLight | ThemeName::Leuven | ThemeName::DoomOneLight)
    }
}

/// Colours for highlighted code: keywords, types, strings, comments, numbers.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Syntax {
    pub keyword: Color32,
    pub kind: Color32,
    pub string: Color32,
    pub comment: Color32,
    /// Numbers, and the constants and escapes that most themes colour alike.
    pub number: Color32,
    /// Function and method names, called or defined.
    pub function: Color32,
    /// Fields, object keys, markup attributes and shell variables.
    pub property: Color32,
    /// Attributes, macros, decorators and lifetimes: what is about the code.
    pub meta: Color32,
}

impl Syntax {
    /// The same colours whatever the theme (only light and dark differ).
    pub fn classic(dark: bool) -> Syntax {
        let c = Color32::from_rgb;
        if dark {
            Syntax { keyword: c(0xc6, 0x92, 0xf2), kind: c(0x5e, 0xc4, 0xd8), string: c(0x9c, 0xcf, 0x7a), comment: c(0x7b, 0x7e, 0x85), number: c(0xf0, 0xa0, 0x6a), function: c(0x6c, 0xb0, 0xf4), property: c(0xe8, 0x8b, 0x8b), meta: c(0xe3, 0xc9, 0x7a) }
        } else {
            Syntax { keyword: c(0x7a, 0x2e, 0xb8), kind: c(0x0b, 0x6e, 0x8a), string: c(0x3a, 0x7d, 0x1e), comment: c(0x7d, 0x83, 0x8c), number: c(0xb0, 0x52, 0x10), function: c(0x1f, 0x5f, 0xbf), property: c(0xb3, 0x26, 0x1e), meta: c(0x8a, 0x6a, 0x00) }
        }
    }

    /// The theme's own: built from its accent and colours, so code fits the window.
    pub fn of(t: ThemeName) -> Syntax {
        let c = Color32::from_rgb;
        let (keyword, kind, string, comment, number, function, property, meta) = match t {
            ThemeName::Midnight => (c(0x5f, 0xb3, 0xe8), c(0x4f, 0xd1, 0xc5), c(0x9f, 0xd3, 0x8a), c(0x6f, 0x73, 0x7a), c(0xf2, 0xa6, 0x6c), c(0xe8, 0xd3, 0x8a), c(0xec, 0x9a, 0x9a), c(0xc3, 0x9a, 0xe8)),
            ThemeName::Graphite => (c(0xf0, 0xa0, 0x40), c(0xe2, 0xc5, 0x8a), c(0xa9, 0xc9, 0x7c), c(0x7a, 0x7a, 0x7e), c(0xc7, 0x9b, 0xec), c(0x7f, 0xb7, 0xe6), c(0xe8, 0xa0, 0xa0), c(0x8f, 0xd3, 0xc6)),
            ThemeName::Lagoon => (c(0x3e, 0xd0, 0xc8), c(0x8c, 0xc8, 0xff), c(0xc3, 0xe8, 0x8d), c(0x6a, 0x82, 0x85), c(0xf7, 0xb4, 0x6b), c(0xff, 0xd5, 0x80), c(0xff, 0x9e, 0x9e), c(0xc3, 0xa6, 0xff)),
            ThemeName::Navy => (c(0x7a, 0xa2, 0xff), c(0x64, 0xd2, 0xff), c(0xa6, 0xda, 0x95), c(0x6a, 0x75, 0x96), c(0xf5, 0xa9, 0x7f), c(0xff, 0xd5, 0x8a), c(0xff, 0x9e, 0xa8), c(0xc8, 0xa6, 0xff)),
            ThemeName::Paper => (c(0x1f, 0x5f, 0xd6), c(0x0e, 0x7c, 0x86), c(0x2f, 0x7d, 0x32), c(0x86, 0x8d, 0x98), c(0xb4, 0x53, 0x09), c(0x6f, 0x42, 0xc1), c(0xb4, 0x23, 0x18), c(0x8a, 0x5a, 0x00)),
            ThemeName::Linen => (c(0x1b, 0x6c, 0xa8), c(0x8a, 0x5a, 0x00), c(0x4f, 0x7a, 0x12), c(0x8f, 0x84, 0x74), c(0xb3, 0x47, 0x1b), c(0x7b, 0x3f, 0xa0), c(0xa8, 0x32, 0x2d), c(0x2c, 0x7a, 0x7b)),
            ThemeName::Mocha => (c(0xcb, 0xa6, 0xf7), c(0xf9, 0xe2, 0xaf), c(0xa6, 0xe3, 0xa1), c(0x7f, 0x84, 0x9c), c(0xfa, 0xb3, 0x87), c(0x89, 0xb4, 0xfa), c(0xb4, 0xbe, 0xfe), c(0x94, 0xe2, 0xd5)),
            ThemeName::TokyoNight => (c(0xbb, 0x9a, 0xf7), c(0x2a, 0xc3, 0xde), c(0x9e, 0xce, 0x6a), c(0x56, 0x5f, 0x89), c(0xff, 0x9e, 0x64), c(0x7a, 0xa2, 0xf7), c(0x73, 0xda, 0xca), c(0xe0, 0xaf, 0x68)),
            ThemeName::Gruvbox => (c(0xfb, 0x49, 0x34), c(0xfa, 0xbd, 0x2f), c(0xb8, 0xbb, 0x26), c(0x92, 0x83, 0x74), c(0xd3, 0x86, 0x9b), c(0x8e, 0xc0, 0x7c), c(0x83, 0xa5, 0x98), c(0xfe, 0x80, 0x19)),
            // Its yellow, green and peach a shade deeper, to read on its light base.
            ThemeName::Latte => (c(0x88, 0x39, 0xef), c(0xb6, 0x74, 0x17), c(0x3c, 0x96, 0x28), c(0x8c, 0x8f, 0xa1), c(0xdf, 0x57, 0x09), c(0x1e, 0x66, 0xf5), c(0x58, 0x68, 0xdc), c(0x17, 0x92, 0x99)),
            ThemeName::Dracula => (c(0xff, 0x79, 0xc6), c(0x8b, 0xe9, 0xfd), c(0xf1, 0xfa, 0x8c), c(0x62, 0x72, 0xa4), c(0xbd, 0x93, 0xf9), c(0x50, 0xfa, 0x7b), c(0xff, 0xb8, 0x6c), c(0xff, 0x55, 0x55)),
            ThemeName::Nord => (c(0x81, 0xa1, 0xc1), c(0x8f, 0xbc, 0xbb), c(0xa3, 0xbe, 0x8c), c(0x61, 0x6e, 0x88), c(0xb4, 0x8e, 0xad), c(0x88, 0xc0, 0xd0), c(0xeb, 0xcb, 0x8b), c(0xd0, 0x87, 0x70)),
            ThemeName::OneDark => (c(0xc6, 0x78, 0xdd), c(0xe5, 0xc0, 0x7b), c(0x98, 0xc3, 0x79), c(0x5c, 0x63, 0x70), c(0xd1, 0x9a, 0x66), c(0x61, 0xaf, 0xef), c(0xe0, 0x6c, 0x75), c(0x56, 0xb6, 0xc2)),
            // The light ones' greens, yellows and cyans a shade deeper, to read on their base.
            ThemeName::SolarizedLight => (c(0x7d, 0x8f, 0x00), c(0xaa, 0x80, 0x00), c(0x26, 0x94, 0x8b), c(0x93, 0xa1, 0xa1), c(0xd3, 0x36, 0x82), c(0x26, 0x8b, 0xd2), c(0x6c, 0x71, 0xc4), c(0xcb, 0x4b, 0x16)),
            ThemeName::RosePineDawn => (c(0x28, 0x69, 0x83), c(0x52, 0x8e, 0x98), c(0xb6, 0x7a, 0x28), c(0x98, 0x93, 0xa5), c(0xbd, 0x72, 0x6e), c(0x90, 0x7a, 0xa9), c(0xb4, 0x63, 0x7a), c(0xb5, 0x60, 0x5c)),
            ThemeName::Everforest => (c(0xe6, 0x7e, 0x80), c(0xdb, 0xbc, 0x7f), c(0xa7, 0xc0, 0x80), c(0x85, 0x92, 0x89), c(0xd6, 0x99, 0xb6), c(0x83, 0xc0, 0x92), c(0x7f, 0xbb, 0xb3), c(0xe6, 0x98, 0x75)),
            ThemeName::Kanagawa => (c(0x95, 0x7f, 0xb8), c(0x7a, 0xa8, 0x9f), c(0x98, 0xbb, 0x6c), c(0x72, 0x71, 0x69), c(0xd2, 0x7e, 0x99), c(0x7e, 0x9c, 0xd8), c(0xe6, 0xc3, 0x84), c(0xff, 0xa0, 0x66)),
            ThemeName::GruvboxLight => (c(0x9d, 0x00, 0x06), c(0xb5, 0x76, 0x14), c(0x79, 0x74, 0x0e), c(0x92, 0x83, 0x74), c(0x8f, 0x3f, 0x71), c(0x42, 0x7b, 0x58), c(0x07, 0x66, 0x78), c(0xaf, 0x3a, 0x03)),
            ThemeName::AyuLight => (c(0xcd, 0x73, 0x32), c(0x34, 0x91, 0xd3), c(0x70, 0x96, 0x00), c(0xa9, 0xaa, 0xad), c(0x9f, 0x77, 0xc7), c(0xb8, 0x78, 0x0f), c(0xd9, 0x53, 0x4f), c(0x24, 0x8a, 0x6a)),
            ThemeName::RosePine => (c(0x31, 0x74, 0x8f), c(0x9c, 0xcf, 0xd8), c(0xf6, 0xc1, 0x77), c(0x6e, 0x6a, 0x86), c(0xc4, 0xa7, 0xe7), c(0xeb, 0xbc, 0xba), c(0xeb, 0x6f, 0x92), c(0xc4, 0xa7, 0xe7)),
            ThemeName::SolarizedDark => (c(0x85, 0x99, 0x00), c(0xb5, 0x89, 0x00), c(0x2a, 0xa1, 0x98), c(0x58, 0x6e, 0x75), c(0xd3, 0x36, 0x82), c(0x26, 0x8b, 0xd2), c(0x6c, 0x71, 0xc4), c(0xcb, 0x4b, 0x16)),
            ThemeName::NightOwl => (c(0xc7, 0x92, 0xea), c(0xff, 0xcb, 0x8b), c(0xec, 0xc4, 0x8d), c(0x63, 0x77, 0x77), c(0xf7, 0x8c, 0x6c), c(0x82, 0xaa, 0xff), c(0x7f, 0xdb, 0xca), c(0xad, 0xdb, 0x67)),
            ThemeName::EverforestLight => (c(0xe9, 0x4f, 0x4d), c(0xad, 0x7c, 0x00), c(0x7e, 0x90, 0x00), c(0x93, 0x9f, 0x91), c(0xcd, 0x60, 0xab), c(0x2a, 0x8a, 0x65), c(0x2f, 0x7f, 0xae), c(0xc2, 0x5a, 0x12)),
            ThemeName::TokyoNightDay => (c(0x98, 0x54, 0xf1), c(0x00, 0x71, 0x97), c(0x58, 0x75, 0x39), c(0x84, 0x8c, 0xb5), c(0xb1, 0x5c, 0x00), c(0x2a, 0x6f, 0xd0), c(0x38, 0x70, 0x68), c(0x8c, 0x6c, 0x3e)),
            ThemeName::KanagawaLotus => (c(0x62, 0x4c, 0x83), c(0x59, 0x7b, 0x75), c(0x6c, 0x86, 0x4c), c(0x8a, 0x89, 0x80), c(0xb3, 0x5b, 0x79), c(0x4d, 0x69, 0x9b), c(0x77, 0x71, 0x3f), c(0xb3, 0x5f, 0x00)),
            ThemeName::CatppuccinFrappe => (c(0xca, 0x9e, 0xe6), c(0xe5, 0xc8, 0x90), c(0xa6, 0xd1, 0x89), c(0x83, 0x8b, 0xa7), c(0xef, 0x9f, 0x76), c(0x8c, 0xaa, 0xee), c(0xba, 0xbb, 0xf1), c(0x81, 0xc8, 0xbe)),
            ThemeName::CatppuccinMacchiato => (c(0xc6, 0xa0, 0xf6), c(0xee, 0xd4, 0x9f), c(0xa6, 0xda, 0x95), c(0x80, 0x87, 0xa2), c(0xf5, 0xa9, 0x7f), c(0x8a, 0xad, 0xf4), c(0xb7, 0xbd, 0xf8), c(0x8b, 0xd5, 0xca)),
            ThemeName::Monokai => (c(0xf9, 0x26, 0x72), c(0x66, 0xd9, 0xef), c(0xe6, 0xdb, 0x74), c(0x75, 0x71, 0x5e), c(0xae, 0x81, 0xff), c(0xa6, 0xe2, 0x2e), c(0xfd, 0x97, 0x1f), c(0x66, 0xd9, 0xef)),
            ThemeName::Palenight => (c(0xc7, 0x92, 0xea), c(0xff, 0xcb, 0x6b), c(0xc3, 0xe8, 0x8d), c(0x67, 0x6e, 0x95), c(0xf7, 0x8c, 0x6c), c(0x82, 0xaa, 0xff), c(0xf0, 0x71, 0x78), c(0x89, 0xdd, 0xff)),
            ThemeName::AyuDark => (c(0xff, 0x8f, 0x40), c(0x59, 0xc2, 0xff), c(0xaa, 0xd9, 0x4c), c(0x56, 0x5b, 0x66), c(0xd2, 0xa6, 0xff), c(0xff, 0xb4, 0x54), c(0xf0, 0x71, 0x78), c(0x95, 0xe6, 0xcb)),
            ThemeName::AyuMirage => (c(0xff, 0xad, 0x66), c(0x73, 0xd0, 0xff), c(0xd5, 0xff, 0x80), c(0x6e, 0x7c, 0x8f), c(0xdf, 0xbf, 0xff), c(0xff, 0xd1, 0x73), c(0xf2, 0x87, 0x79), c(0x95, 0xe6, 0xcb)),
            ThemeName::RosePineMoon => (c(0x3e, 0x8f, 0xb0), c(0x9c, 0xcf, 0xd8), c(0xf6, 0xc1, 0x77), c(0x6e, 0x6a, 0x86), c(0xc4, 0xa7, 0xe7), c(0xea, 0x9a, 0x97), c(0xeb, 0x6f, 0x92), c(0xc4, 0xa7, 0xe7)),
            ThemeName::TokyoNightStorm => (c(0xbb, 0x9a, 0xf7), c(0x2a, 0xc3, 0xde), c(0x9e, 0xce, 0x6a), c(0x56, 0x5f, 0x89), c(0xff, 0x9e, 0x64), c(0x7a, 0xa2, 0xf7), c(0x73, 0xda, 0xca), c(0xe0, 0xaf, 0x68)),
            ThemeName::KanagawaDragon => (c(0x89, 0x92, 0xa7), c(0x8e, 0xa4, 0xa2), c(0x8a, 0x9a, 0x7b), c(0x73, 0x7c, 0x73), c(0xa2, 0x92, 0xa3), c(0x8b, 0xa4, 0xb0), c(0xc4, 0xb2, 0x8a), c(0xb6, 0x92, 0x7b)),
            ThemeName::Nightfox => (c(0x9d, 0x79, 0xd6), c(0xdb, 0xc0, 0x74), c(0x81, 0xb2, 0x9a), c(0x73, 0x80, 0x91), c(0xf4, 0xa2, 0x61), c(0x71, 0x9c, 0xd6), c(0x63, 0xcd, 0xcf), c(0xc9, 0x4f, 0x6d)),
            ThemeName::FlexokiDark => (c(0x87, 0x9a, 0x39), c(0xd0, 0xa2, 0x15), c(0x3a, 0xa9, 0x9f), c(0x6f, 0x6e, 0x69), c(0x8b, 0x7e, 0xc8), c(0xda, 0x70, 0x2c), c(0x43, 0x85, 0xbe), c(0xce, 0x5d, 0x97)),
            ThemeName::OneLight => (c(0xa6, 0x26, 0xa4), c(0xb9, 0x7e, 0x00), c(0x4c, 0x9a, 0x4b), c(0xa0, 0xa1, 0xa7), c(0x98, 0x68, 0x01), c(0x40, 0x78, 0xf2), c(0xe4, 0x56, 0x49), c(0x01, 0x84, 0xbc)),
            ThemeName::LightOwl => (c(0x99, 0x4c, 0xc3), c(0x0c, 0x96, 0x9b), c(0xc9, 0x67, 0x65), c(0x98, 0x9f, 0xb1), c(0xaa, 0x09, 0x82), c(0x48, 0x76, 0xd6), c(0xbc, 0x54, 0x54), c(0x1f, 0x8a, 0x80)),
            ThemeName::Dayfox => (c(0x6e, 0x33, 0xce), c(0xac, 0x54, 0x02), c(0x39, 0x68, 0x47), c(0x83, 0x7a, 0x72), c(0x95, 0x5f, 0x61), c(0x28, 0x48, 0xa9), c(0x28, 0x79, 0x80), c(0xa5, 0x22, 0x2f)),
            ThemeName::FlexokiLight => (c(0x66, 0x80, 0x0b), c(0xad, 0x83, 0x01), c(0x24, 0x83, 0x7b), c(0x87, 0x85, 0x80), c(0x5e, 0x40, 0x9d), c(0xbc, 0x52, 0x15), c(0x20, 0x5e, 0xa6), c(0xa0, 0x2f, 0x6f)),
            ThemeName::GitHubDark => (c(0xff, 0x7b, 0x72), c(0xff, 0xa6, 0x57), c(0xa5, 0xd6, 0xff), c(0x8b, 0x94, 0x9e), c(0x79, 0xc0, 0xff), c(0xd2, 0xa8, 0xff), c(0x79, 0xc0, 0xff), c(0x7e, 0xe7, 0x87)),
            ThemeName::GitHubDimmed => (c(0xf4, 0x70, 0x67), c(0xf6, 0x9d, 0x50), c(0x96, 0xd0, 0xff), c(0x76, 0x83, 0x90), c(0x6c, 0xb6, 0xff), c(0xdc, 0xbd, 0xfb), c(0x6c, 0xb6, 0xff), c(0x8d, 0xdb, 0x8c)),
            ThemeName::Zenburn => (c(0xf0, 0xdf, 0xaf), c(0x7c, 0xb8, 0xbb), c(0xcc, 0x93, 0x93), c(0x7f, 0x9f, 0x7f), c(0x8c, 0xd0, 0xd3), c(0xef, 0xef, 0x8f), c(0xdf, 0xaf, 0x8f), c(0xdc, 0x8c, 0xc3)),
            ThemeName::TomorrowNight => (c(0xb2, 0x94, 0xbb), c(0xf0, 0xc6, 0x74), c(0xb5, 0xbd, 0x68), c(0x96, 0x98, 0x96), c(0xde, 0x93, 0x5f), c(0x81, 0xa2, 0xbe), c(0xcc, 0x66, 0x66), c(0x8a, 0xbe, 0xb7)),
            ThemeName::OceanicNext => (c(0xc5, 0x94, 0xc5), c(0xfa, 0xc8, 0x63), c(0x99, 0xc7, 0x94), c(0x65, 0x73, 0x7e), c(0xf9, 0x91, 0x57), c(0x66, 0x99, 0xcc), c(0xec, 0x5f, 0x67), c(0x5f, 0xb3, 0xb3)),
            ThemeName::Horizon => (c(0xb8, 0x77, 0xdb), c(0x25, 0xb0, 0xbc), c(0xfa, 0xb7, 0x95), c(0x6c, 0x6f, 0x93), c(0xf0, 0x94, 0x83), c(0x59, 0xe3, 0xe3), c(0xe9, 0x56, 0x78), c(0xfa, 0xc2, 0x9a)),
            ThemeName::Cobalt2 => (c(0xff, 0x9d, 0x00), c(0x80, 0xff, 0xbb), c(0xa5, 0xff, 0x90), c(0x00, 0x88, 0xff), c(0xff, 0x62, 0x8c), c(0xff, 0xc6, 0x00), c(0x9e, 0xff, 0xff), c(0xfb, 0x94, 0xff)),
            ThemeName::Synthwave => (c(0xfe, 0xde, 0x5d), c(0x72, 0xf1, 0xb8), c(0xff, 0x8b, 0x39), c(0x84, 0x8b, 0xbd), c(0xf9, 0x7e, 0x72), c(0x36, 0xf9, 0xf6), c(0xff, 0x7e, 0xdb), c(0xfe, 0x44, 0x50)),
            ThemeName::Moonlight => (c(0xc0, 0x99, 0xff), c(0xff, 0xc7, 0x77), c(0xc3, 0xe8, 0x8d), c(0x7a, 0x88, 0xcf), c(0xff, 0x96, 0x6c), c(0x82, 0xaa, 0xff), c(0x4f, 0xd6, 0xbe), c(0x86, 0xe1, 0xfc)),
            ThemeName::Poimandres => (c(0x91, 0xb4, 0xd5), c(0xad, 0xd7, 0xff), c(0x5d, 0xe4, 0xc7), c(0x76, 0x7c, 0x9d), c(0xfc, 0xc5, 0xe9), c(0x89, 0xdd, 0xff), c(0xd0, 0x67, 0x9d), c(0xff, 0xfa, 0xc2)),
            ThemeName::Oxocarbon => (c(0xbe, 0x95, 0xff), c(0x3d, 0xdb, 0xd9), c(0x33, 0xb1, 0xff), c(0x6f, 0x6f, 0x6f), c(0xff, 0x7e, 0xb6), c(0x82, 0xcf, 0xff), c(0x78, 0xa9, 0xff), c(0x42, 0xbe, 0x65)),
            ThemeName::ModusVivendi => (c(0xb6, 0xa0, 0xff), c(0x6a, 0xe4, 0xb9), c(0x79, 0xa8, 0xff), c(0x98, 0x98, 0x98), c(0x00, 0xbc, 0xff), c(0xfe, 0xac, 0xd0), c(0x00, 0xd3, 0xd0), c(0xd0, 0xbc, 0x00)),
            ThemeName::Iceberg => (c(0x84, 0xa0, 0xc6), c(0xe2, 0xa4, 0x78), c(0x89, 0xb8, 0xc2), c(0x6b, 0x70, 0x89), c(0xa0, 0x93, 0xc7), c(0xb4, 0xbe, 0x82), c(0xe2, 0x78, 0x78), c(0x95, 0xc4, 0xce)),
            ThemeName::GitHubLight => (c(0xcf, 0x22, 0x2e), c(0x95, 0x38, 0x00), c(0x0a, 0x30, 0x69), c(0x6e, 0x77, 0x81), c(0x05, 0x50, 0xae), c(0x82, 0x50, 0xdf), c(0x05, 0x50, 0xae), c(0x11, 0x63, 0x29)),
            // Its yellow and orange a shade deeper, to read on white.
            ThemeName::Tomorrow => (c(0x89, 0x59, 0xa8), c(0x9a, 0x75, 0x00), c(0x71, 0x8c, 0x00), c(0x8e, 0x90, 0x8c), c(0xc7, 0x6a, 0x10), c(0x42, 0x71, 0xae), c(0xc8, 0x28, 0x29), c(0x3e, 0x99, 0x9f)),
            // Its orange, yellow or cyan a shade deeper, to read on its base.
            ThemeName::IcebergLight => (c(0x2d, 0x53, 0x9e), c(0xb5, 0x65, 0x2c), c(0x3f, 0x83, 0xa6), c(0x83, 0x89, 0xa3), c(0x77, 0x59, 0xb4), c(0x58, 0x7c, 0x33), c(0xcc, 0x51, 0x7a), c(0x22, 0x77, 0x8a)),
            ThemeName::ModusOperandi => (c(0x53, 0x17, 0xac), c(0x00, 0x5a, 0x5f), c(0x25, 0x44, 0xbb), c(0x59, 0x59, 0x59), c(0x00, 0x00, 0xc0), c(0x72, 0x10, 0x45), c(0x00, 0x5e, 0x8b), c(0x6f, 0x55, 0x00)),
            // Its orange, yellow or cyan a shade deeper, to read on its base.
            ThemeName::SelenizedLight => (c(0x87, 0x62, 0xc6), c(0x8f, 0x71, 0x00), c(0x00, 0x85, 0x7a), c(0x90, 0x99, 0x95), c(0xc2, 0x5d, 0x1e), c(0x00, 0x64, 0xe4), c(0xd2, 0x21, 0x2d), c(0xca, 0x48, 0x98)),
            ThemeName::QuietLight => (c(0x4b, 0x69, 0xc6), c(0x7a, 0x3e, 0x9d), c(0x44, 0x8c, 0x27), c(0xaa, 0xaa, 0xaa), c(0xab, 0x65, 0x26), c(0xaa, 0x37, 0x31), c(0x0b, 0x7e, 0x8a), c(0xa6, 0x5e, 0x00)),
            ThemeName::PaperColor => (c(0xd7, 0x00, 0x87), c(0x00, 0x5f, 0x87), c(0x5f, 0x87, 0x00), c(0x87, 0x87, 0x87), c(0xd7, 0x5f, 0x00), c(0x00, 0x5f, 0xaf), c(0xaf, 0x00, 0x00), c(0x87, 0x00, 0xaf)),
            ThemeName::DarkModern => (c(0x56, 0x9c, 0xd6), c(0x4e, 0xc9, 0xb0), c(0xce, 0x91, 0x78), c(0x6a, 0x99, 0x55), c(0xb5, 0xce, 0xa8), c(0xdc, 0xdc, 0xaa), c(0x9c, 0xdc, 0xfe), c(0xc5, 0x86, 0xc0)),
            ThemeName::Darcula => (c(0xcc, 0x78, 0x32), c(0xff, 0xc6, 0x6d), c(0x6a, 0x87, 0x59), c(0x80, 0x80, 0x80), c(0x68, 0x97, 0xbb), c(0xff, 0xc6, 0x6d), c(0x98, 0x76, 0xaa), c(0xbb, 0xb5, 0x29)),
            ThemeName::XcodeDark => (c(0xfc, 0x5f, 0xa3), c(0xd0, 0xa8, 0xff), c(0xfc, 0x6a, 0x5d), c(0x6c, 0x79, 0x86), c(0xd0, 0xbf, 0x69), c(0x67, 0xb7, 0xa4), c(0x41, 0xa1, 0xc0), c(0xfd, 0x8f, 0x3f)),
            ThemeName::MaterialDarker => (c(0xc7, 0x92, 0xea), c(0xff, 0xcb, 0x6b), c(0xc3, 0xe8, 0x8d), c(0x61, 0x61, 0x61), c(0xf7, 0x8c, 0x6c), c(0x82, 0xaa, 0xff), c(0xf0, 0x71, 0x78), c(0x89, 0xdd, 0xff)),
            ThemeName::DoomOne => (c(0x51, 0xaf, 0xef), c(0xec, 0xbe, 0x7b), c(0x98, 0xbe, 0x65), c(0x5b, 0x62, 0x68), c(0xda, 0x85, 0x48), c(0xc6, 0x78, 0xdd), c(0xdc, 0xae, 0xea), c(0x46, 0xd9, 0xff)),
            ThemeName::Sonokai => (c(0xfc, 0x5d, 0x7c), c(0x76, 0xcc, 0xe0), c(0xe7, 0xc6, 0x64), c(0x7f, 0x84, 0x90), c(0xb3, 0x9d, 0xf3), c(0x9e, 0xd0, 0x72), c(0xf3, 0x96, 0x60), c(0xb3, 0x9d, 0xf3)),
            ThemeName::GruvboxMaterial => (c(0xea, 0x69, 0x62), c(0xd8, 0xa6, 0x57), c(0x89, 0xb4, 0x82), c(0x92, 0x83, 0x74), c(0xd3, 0x86, 0x9b), c(0xa9, 0xb6, 0x65), c(0x7d, 0xae, 0xa3), c(0xe7, 0x8a, 0x4e)),
            ThemeName::Vesper => (c(0xa0, 0xa0, 0xa0), c(0xff, 0xc7, 0x99), c(0x99, 0xff, 0xe4), c(0x57, 0x57, 0x57), c(0xff, 0xc7, 0x99), c(0xff, 0xc7, 0x99), c(0xc2, 0xc2, 0xc2), c(0x99, 0xff, 0xe4)),
            ThemeName::Andromeda => (c(0xc7, 0x4d, 0xed), c(0xff, 0xe6, 0x6d), c(0x96, 0xe0, 0x72), c(0x87, 0x88, 0x8f), c(0xf3, 0x9c, 0x12), c(0xff, 0xe6, 0x6d), c(0x00, 0xe8, 0xc6), c(0xee, 0x5d, 0x43)),
            ThemeName::ShadesOfPurple => (c(0xff, 0x9d, 0x00), c(0xfb, 0x94, 0xff), c(0xa5, 0xff, 0x90), c(0xb3, 0x62, 0xff), c(0xff, 0x62, 0x8c), c(0xfa, 0xd0, 0x00), c(0x9e, 0xff, 0xff), c(0x80, 0xff, 0xbb)),
            ThemeName::Carbonfox => (c(0xbe, 0x95, 0xff), c(0x08, 0xbd, 0xba), c(0x25, 0xbe, 0x6a), c(0x6e, 0x6f, 0x70), c(0x3d, 0xdb, 0xd9), c(0x78, 0xa9, 0xff), c(0x33, 0xb1, 0xff), c(0xee, 0x53, 0x96)),
            ThemeName::Duskfox => (c(0xc4, 0xa7, 0xe7), c(0xf6, 0xc1, 0x77), c(0xa3, 0xbe, 0x8c), c(0x81, 0x7c, 0x9c), c(0xea, 0x9a, 0x97), c(0x56, 0x9f, 0xba), c(0x9c, 0xcf, 0xd8), c(0xeb, 0x6f, 0x92)),
            ThemeName::VitesseDark => (c(0x4d, 0x93, 0x75), c(0x5d, 0xa9, 0x94), c(0xc9, 0x8a, 0x7d), c(0x68, 0x76, 0x68), c(0x4c, 0x9a, 0x91), c(0x80, 0xa6, 0x65), c(0xb8, 0xa9, 0x65), c(0xcb, 0x76, 0x76)),
            ThemeName::LightModern => (c(0x00, 0x00, 0xff), c(0x26, 0x7f, 0x99), c(0xa3, 0x15, 0x15), c(0x00, 0x80, 0x00), c(0x09, 0x86, 0x58), c(0x79, 0x5e, 0x26), c(0x00, 0x10, 0x80), c(0xaf, 0x00, 0xdb)),
            ThemeName::IntelliJLight => (c(0x00, 0x33, 0xb3), c(0x00, 0x62, 0x7a), c(0x06, 0x7d, 0x17), c(0x8c, 0x8c, 0x8c), c(0x17, 0x50, 0xeb), c(0x00, 0x62, 0x7a), c(0x87, 0x10, 0x94), c(0x9e, 0x88, 0x0d)),
            ThemeName::XcodeLight => (c(0x9b, 0x23, 0x93), c(0x39, 0x00, 0xa0), c(0xc4, 0x1a, 0x16), c(0x5d, 0x6c, 0x79), c(0x1c, 0x00, 0xcf), c(0x32, 0x6d, 0x74), c(0x6c, 0x36, 0xa9), c(0x64, 0x38, 0x20)),
            // Its yellow and orange a shade deeper, to read on its base.
            ThemeName::Dawnfox => (c(0x90, 0x7a, 0xa9), c(0xb6, 0x7a, 0x28), c(0x61, 0x87, 0x74), c(0x98, 0x93, 0xa5), c(0xbd, 0x72, 0x6e), c(0x28, 0x69, 0x83), c(0x4a, 0x85, 0x90), c(0xb4, 0x63, 0x7a)),
            ThemeName::VitesseLight => (c(0x1e, 0x75, 0x4f), c(0x2e, 0x8f, 0x82), c(0xb5, 0x69, 0x59), c(0xa0, 0xad, 0xa0), c(0x2f, 0x79, 0x8a), c(0x59, 0x87, 0x3a), c(0x99, 0x84, 0x18), c(0xab, 0x59, 0x59)),
            ThemeName::GruvboxMaterialLight => (c(0xc1, 0x4a, 0x4a), c(0xb4, 0x71, 0x09), c(0x4c, 0x7a, 0x5d), c(0x92, 0x83, 0x74), c(0x94, 0x5e, 0x80), c(0x6c, 0x78, 0x2e), c(0x45, 0x70, 0x7a), c(0xc3, 0x5e, 0x0a)),
            ThemeName::Alabaster => (c(0x00, 0x00, 0x00), c(0x32, 0x5c, 0xc0), c(0x44, 0x8c, 0x27), c(0xaa, 0x37, 0x31), c(0x7a, 0x3e, 0x9d), c(0x32, 0x5c, 0xc0), c(0x00, 0x00, 0x00), c(0x7a, 0x3e, 0x9d)),
            ThemeName::MonokaiPro => (c(0xff, 0x61, 0x88), c(0x78, 0xdc, 0xe8), c(0xff, 0xd8, 0x66), c(0x72, 0x70, 0x72), c(0xab, 0x9d, 0xf2), c(0xa9, 0xdc, 0x76), c(0xfc, 0x98, 0x67), c(0x78, 0xdc, 0xe8)),
            ThemeName::Everblush => (c(0xc4, 0x7f, 0xd5), c(0x67, 0xb0, 0xe8), c(0x8c, 0xcf, 0x7e), c(0x5f, 0x69, 0x6d), c(0xe5, 0xc7, 0x6b), c(0x6c, 0xbf, 0xbf), c(0xe5, 0x74, 0x74), c(0xe6, 0x98, 0x75)),
            ThemeName::Jellybeans => (c(0x81, 0x97, 0xbf), c(0xff, 0xb9, 0x64), c(0x99, 0xad, 0x6a), c(0x88, 0x88, 0x88), c(0xcf, 0x6a, 0x4c), c(0xfa, 0xd0, 0x7a), c(0xc6, 0xb6, 0xee), c(0x8f, 0xbf, 0xdc)),
            ThemeName::Srcery => (c(0xef, 0x2f, 0x27), c(0xfb, 0xb8, 0x29), c(0x51, 0x9f, 0x50), c(0x91, 0x81, 0x75), c(0xff, 0x5f, 0x00), c(0x2c, 0x78, 0xbf), c(0x0a, 0xae, 0xb3), c(0xe0, 0x2c, 0x6d)),
            ThemeName::Terafox => (c(0xad, 0x5c, 0x7c), c(0xfd, 0xa4, 0x7f), c(0x7a, 0xa4, 0xa1), c(0x6d, 0x7f, 0x8b), c(0xff, 0x83, 0x49), c(0x5a, 0x93, 0xaa), c(0xa1, 0xcd, 0xd8), c(0xe8, 0x5c, 0x51)),
            ThemeName::EdgeDark => (c(0xd3, 0x8a, 0xea), c(0xde, 0xb9, 0x74), c(0xa0, 0xc9, 0x80), c(0x7f, 0x84, 0x90), c(0x5d, 0xbb, 0xc1), c(0x6c, 0xb6, 0xeb), c(0xec, 0x72, 0x79), c(0x5d, 0xbb, 0xc1)),
            ThemeName::Eighties => (c(0xcc, 0x99, 0xcc), c(0xff, 0xcc, 0x66), c(0x99, 0xcc, 0x99), c(0x99, 0x99, 0x99), c(0xf9, 0x91, 0x57), c(0x66, 0x99, 0xcc), c(0xf2, 0x77, 0x7a), c(0x66, 0xcc, 0xcc)),
            ThemeName::Panda => (c(0xff, 0x75, 0xb5), c(0xff, 0xb8, 0x6c), c(0x19, 0xf9, 0xd8), c(0x67, 0x6b, 0x79), c(0xb0, 0x84, 0xeb), c(0x6f, 0xc1, 0xff), c(0xff, 0x9a, 0xc1), c(0xff, 0xcc, 0x95)),
            ThemeName::Cyberdream => (c(0xbd, 0x5e, 0xff), c(0x5e, 0xf1, 0xff), c(0x5e, 0xff, 0x6c), c(0x7b, 0x84, 0x96), c(0xff, 0xbd, 0x5e), c(0x5e, 0xa1, 0xff), c(0xff, 0x5e, 0xf1), c(0xff, 0x6e, 0x5e)),
            ThemeName::Mariana => (c(0xc6, 0x95, 0xc6), c(0xfa, 0xc7, 0x61), c(0x99, 0xc7, 0x94), c(0xa6, 0xac, 0xb9), c(0xf9, 0xae, 0x58), c(0x5f, 0xb4, 0xb4), c(0xec, 0x5f, 0x66), c(0x66, 0x99, 0xcc)),
            ThemeName::Embark => (c(0xd4, 0xbf, 0xff), c(0x91, 0xdd, 0xff), c(0xa1, 0xef, 0xd3), c(0x8a, 0x88, 0x9d), c(0xff, 0xe6, 0xb3), c(0x78, 0xa8, 0xff), c(0xf4, 0x8f, 0xb1), c(0x63, 0xf2, 0xf1)),
            ThemeName::SpacemacsDark => (c(0x4f, 0x97, 0xd7), c(0xce, 0x53, 0x7a), c(0x2d, 0x95, 0x74), c(0x2a, 0xa1, 0xae), c(0xa4, 0x5b, 0xad), c(0xbc, 0x6e, 0xc5), c(0x75, 0x90, 0xdb), c(0xb1, 0x95, 0x1d)),
            ThemeName::MelangeDark => (c(0xe4, 0x9b, 0x5d), c(0x89, 0xb3, 0xb6), c(0xa3, 0xa9, 0xce), c(0xc1, 0xa7, 0x8e), c(0xcf, 0x9b, 0xc2), c(0xeb, 0xc0, 0x6d), c(0x85, 0xb6, 0x95), c(0xd4, 0x77, 0x66)),
            ThemeName::EdgeLight => (c(0xb0, 0x5c, 0xcc), c(0xbe, 0x7e, 0x05), c(0x60, 0x8e, 0x32), c(0x87, 0x90, 0xa0), c(0x3a, 0x8b, 0x84), c(0x50, 0x79, 0xbe), c(0xd0, 0x58, 0x58), c(0x3a, 0x8b, 0x84)),
            ThemeName::MonokaiProLight => (c(0xe1, 0x47, 0x75), c(0x1c, 0x8c, 0xa8), c(0xcc, 0x7a, 0x0a), c(0xa5, 0x9f, 0xa0), c(0x70, 0x58, 0xbe), c(0x1f, 0x8a, 0x5b), c(0xcf, 0x54, 0x26), c(0x1c, 0x8c, 0xa8)),
            ThemeName::Alucard => (c(0xa3, 0x14, 0x4d), c(0x03, 0x6a, 0x96), c(0x84, 0x6e, 0x15), c(0x63, 0x5d, 0x97), c(0x64, 0x4a, 0xc9), c(0x14, 0x71, 0x0a), c(0xa3, 0x4d, 0x14), c(0xcb, 0x3a, 0x2a)),
            ThemeName::Zenbones => (c(0x88, 0x50, 0x7d), c(0x28, 0x64, 0x86), c(0x4f, 0x6c, 0x31), c(0x8e, 0x81, 0x7b), c(0x94, 0x49, 0x27), c(0x3b, 0x5e, 0x8a), c(0xa8, 0x33, 0x4c), c(0x6c, 0x5b, 0x3c)),
            ThemeName::SpacemacsLight => (c(0x3a, 0x81, 0xc3), c(0xba, 0x2f, 0x59), c(0x2d, 0x95, 0x74), c(0x2a, 0xa1, 0xae), c(0x4e, 0x31, 0x63), c(0x6c, 0x31, 0x63), c(0x71, 0x5a, 0xb1), c(0x8f, 0x7a, 0x10)),
            ThemeName::Leuven => (c(0x00, 0x00, 0xff), c(0x64, 0x34, 0xa3), c(0x00, 0x80, 0x00), c(0x8d, 0x8d, 0x84), c(0xd0, 0x37, 0x2d), c(0x00, 0x66, 0x99), c(0xba, 0x36, 0xa5), c(0x8b, 0x45, 0x13)),
            ThemeName::DoomOneLight => (c(0xe4, 0x56, 0x49), c(0x98, 0x68, 0x01), c(0x50, 0xa1, 0x4f), c(0x9c, 0xa0, 0xa4), c(0xb7, 0x51, 0xb6), c(0x40, 0x78, 0xf2), c(0x01, 0x84, 0xbc), c(0xa8, 0x72, 0x00)),
        };
        Syntax { keyword, kind, string, comment, number, function, property, meta }
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub panel: Color32,
    pub sidebar: Color32,
    pub raised: Color32,
    pub hover: Color32,
    pub selected: Color32,
    pub cursor: Color32,
    pub accent: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub border: Color32,
    pub danger: Color32,
    /// Title strip behind the tabs (the active tab takes the content color).
    pub chrome: Color32,
    /// Alternate (striped) rows.
    pub alt: Color32,
    /// Hovered list rows: a tint of the selection color.
    pub row_hover: Color32,
    /// Highlighted code (the theme's own, or the classic set: `Syntax`).
    pub syntax: Syntax,
}

impl Palette {
    /// Text on a selected row: white on a deep selection colour, the theme's own text on
    /// a pale one (white there all but disappears).
    pub fn on_sel(&self) -> Color32 {
        let lin = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        let s = self.selected;
        let lum = 0.2126 * lin(s.r()) + 0.7152 * lin(s.g()) + 0.0722 * lin(s.b());
        if lum < 0.3 { Color32::WHITE } else { self.text }
    }

    /// A quiet card inside a popup (a group of rows): a step from the popup towards the
    /// window, as Options draws its groups.
    pub fn card(&self) -> Color32 {
        self.raised.lerp_to_gamma(self.bg, 0.45)
    }

    pub fn card_line(&self) -> Color32 {
        self.border.gamma_multiply(0.8)
    }

    /// Secondary text on a selected row: `on_sel` a little blended into the selection. The
    /// dim grey used elsewhere sinks into a saturated selection.
    pub fn on_sel_dim(&self) -> Color32 {
        self.on_sel().lerp_to_gamma(self.selected, 0.3)
    }
}

pub fn palette(t: ThemeName) -> Palette {
    let mut p = base_palette(t);
    let mix = |a: Color32, b: Color32, t: f32| Color32::from_rgb((a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8, (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8, (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8);
    if t == ThemeName::Midnight {
        // Neutral graphite: a lighter title strip, the sidebar a step above the list,
        // clearly visible stripes and a saturated teal selection.
        p.chrome = Color32::from_rgb(0x26, 0x27, 0x2a);
        p.alt = Color32::from_rgb(0x1d, 0x1e, 0x21);
    } else {
        p.chrome = mix(p.sidebar, p.text, if p.dark { 0.07 } else { 0.05 });
        p.alt = mix(p.panel, p.text, if p.dark { 0.035 } else { 0.03 });
    }
    p.row_hover = mix(p.panel, p.selected, if p.dark { 0.35 } else { 0.22 });
    p.syntax = Syntax::of(t);
    p
}

fn base_palette(t: ThemeName) -> Palette {
    let c = Color32::from_rgb;
    let z = Color32::TRANSPARENT;
    match t {
        ThemeName::Midnight => Palette {
            dark: true,
            bg: c(0x18, 0x19, 0x1c),
            panel: c(0x15, 0x16, 0x19),
            sidebar: c(0x1d, 0x1e, 0x21),
            raised: c(0x25, 0x26, 0x2a),
            hover: c(0x27, 0x29, 0x2d),
            selected: c(0x12, 0x79, 0xa0),
            cursor: c(0x10, 0x88, 0xba),
            accent: c(0x1a, 0x91, 0xc8),
            text: c(0xde, 0xdf, 0xe1),
            text_dim: c(0x8c, 0x8f, 0x95),
            border: c(0x2c, 0x2d, 0x31),
            danger: c(0xff, 0x6b, 0x6b),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Graphite => Palette {
            dark: true,
            bg: c(0x1e, 0x1e, 0x1e),
            panel: c(0x25, 0x25, 0x26),
            sidebar: c(0x19, 0x19, 0x19),
            raised: c(0x2f, 0x2f, 0x31),
            hover: c(0x33, 0x33, 0x36),
            selected: c(0x3a, 0x3d, 0x41),
            cursor: c(0x4a, 0x4e, 0x55),
            accent: c(0xf0, 0xa0, 0x40),
            text: c(0xe8, 0xe8, 0xe8),
            text_dim: c(0x95, 0x95, 0x98),
            border: c(0x34, 0x34, 0x36),
            danger: c(0xff, 0x6b, 0x6b),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Lagoon => Palette {
            dark: true,
            bg: c(0x11, 0x1b, 0x1d),
            panel: c(0x16, 0x22, 0x25),
            sidebar: c(0x0e, 0x17, 0x19),
            raised: c(0x1e, 0x2d, 0x31),
            hover: c(0x21, 0x33, 0x37),
            selected: c(0x17, 0x4a, 0x4f),
            cursor: c(0x1d, 0x5e, 0x64),
            accent: c(0x3e, 0xd0, 0xc8),
            text: c(0xe2, 0xec, 0xec),
            text_dim: c(0x86, 0x9c, 0x9e),
            border: c(0x25, 0x35, 0x39),
            danger: c(0xff, 0x6b, 0x6b),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Navy => Palette {
            dark: true,
            bg: c(0x10, 0x16, 0x26),
            panel: c(0x15, 0x1c, 0x30),
            sidebar: c(0x0d, 0x12, 0x20),
            raised: c(0x1d, 0x26, 0x3e),
            hover: c(0x22, 0x2c, 0x47),
            selected: c(0x25, 0x3a, 0x6e),
            cursor: c(0x2e, 0x48, 0x88),
            accent: c(0x7a, 0xa2, 0xff),
            text: c(0xe3, 0xe8, 0xf4),
            text_dim: c(0x8a, 0x94, 0xb0),
            border: c(0x25, 0x2f, 0x4a),
            danger: c(0xff, 0x6b, 0x6b),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Paper => Palette {
            dark: false,
            bg: c(0xf7, 0xf8, 0xfa),
            panel: c(0xff, 0xff, 0xff),
            sidebar: c(0xee, 0xf0, 0xf3),
            raised: c(0xe8, 0xeb, 0xef),
            hover: c(0xe9, 0xee, 0xf6),
            selected: c(0xd4, 0xe4, 0xfb),
            cursor: c(0xbf, 0xd6, 0xf8),
            accent: c(0x1f, 0x6f, 0xeb),
            text: c(0x1c, 0x1f, 0x24),
            text_dim: c(0x6a, 0x72, 0x7f),
            border: c(0xdc, 0xe0, 0xe6),
            danger: c(0xd6, 0x33, 0x33),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Linen => Palette {
            dark: false,
            bg: c(0xf6, 0xf1, 0xe7),
            panel: c(0xfb, 0xf7, 0xef),
            sidebar: c(0xee, 0xe7, 0xd9),
            raised: c(0xe9, 0xe1, 0xd1),
            hover: c(0xec, 0xe3, 0xd2),
            selected: c(0xd9, 0xe7, 0xf2),
            cursor: c(0xc3, 0xda, 0xee),
            accent: c(0x1b, 0x7a, 0xb3),
            text: c(0x2b, 0x26, 0x20),
            text_dim: c(0x76, 0x6c, 0x5f),
            border: c(0xdd, 0xd3, 0xc1),
            danger: c(0xc8, 0x32, 0x32),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Mocha => Palette {
            dark: true,
            bg: c(0x18, 0x18, 0x25),
            panel: c(0x1e, 0x1e, 0x2e),
            sidebar: c(0x18, 0x18, 0x25),
            raised: c(0x26, 0x26, 0x38),
            hover: c(0x31, 0x32, 0x44),
            selected: c(0x3e, 0x3a, 0x5e),
            cursor: c(0x4d, 0x47, 0x75),
            accent: c(0xcb, 0xa6, 0xf7),
            text: c(0xcd, 0xd6, 0xf4),
            text_dim: c(0x93, 0x99, 0xb2),
            border: c(0x31, 0x32, 0x44),
            danger: c(0xf3, 0x8b, 0xa8),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::TokyoNight => Palette {
            dark: true,
            bg: c(0x16, 0x16, 0x1e),
            panel: c(0x1a, 0x1b, 0x26),
            sidebar: c(0x16, 0x16, 0x1e),
            raised: c(0x24, 0x28, 0x3b),
            hover: c(0x29, 0x2e, 0x42),
            selected: c(0x28, 0x34, 0x57),
            cursor: c(0x36, 0x4a, 0x82),
            accent: c(0x7a, 0xa2, 0xf7),
            text: c(0xc0, 0xca, 0xf5),
            text_dim: c(0x78, 0x7c, 0x99),
            border: c(0x29, 0x2e, 0x42),
            danger: c(0xf7, 0x76, 0x8e),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Gruvbox => Palette {
            dark: true,
            bg: c(0x1d, 0x20, 0x21),
            panel: c(0x28, 0x28, 0x28),
            sidebar: c(0x1d, 0x20, 0x21),
            raised: c(0x32, 0x30, 0x2f),
            hover: c(0x3c, 0x38, 0x36),
            selected: c(0x50, 0x49, 0x45),
            cursor: c(0x66, 0x5c, 0x54),
            accent: c(0xd7, 0x99, 0x21),
            text: c(0xeb, 0xdb, 0xb2),
            text_dim: c(0xa8, 0x99, 0x84),
            border: c(0x3c, 0x38, 0x36),
            danger: c(0xfb, 0x49, 0x34),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Latte => Palette {
            dark: false,
            bg: c(0xe6, 0xe9, 0xef),
            panel: c(0xef, 0xf1, 0xf5),
            sidebar: c(0xe6, 0xe9, 0xef),
            raised: c(0xdc, 0xe0, 0xe8),
            hover: c(0xe1, 0xe4, 0xed),
            selected: c(0xdd, 0xd3, 0xf6),
            cursor: c(0xcc, 0xbd, 0xf2),
            accent: c(0x88, 0x39, 0xef),
            text: c(0x4c, 0x4f, 0x69),
            text_dim: c(0x6c, 0x6f, 0x85),
            border: c(0xcc, 0xd0, 0xda),
            danger: c(0xd2, 0x0f, 0x39),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::Dracula => Palette {
            dark: true,
            bg: c(0x21, 0x22, 0x2c),
            panel: c(0x28, 0x2a, 0x36),
            sidebar: c(0x21, 0x22, 0x2c),
            raised: c(0x34, 0x37, 0x46),
            hover: c(0x3a, 0x3d, 0x4e),
            selected: c(0x44, 0x47, 0x5a),
            cursor: c(0x56, 0x5a, 0x72),
            accent: c(0xbd, 0x93, 0xf9),
            text: c(0xf8, 0xf8, 0xf2),
            text_dim: c(0x9e, 0xa2, 0xbd),
            border: c(0x34, 0x37, 0x46),
            danger: c(0xff, 0x55, 0x55),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Nord => Palette {
            dark: true,
            bg: c(0x27, 0x2c, 0x36),
            panel: c(0x2e, 0x34, 0x40),
            sidebar: c(0x29, 0x2e, 0x39),
            raised: c(0x3b, 0x42, 0x52),
            hover: c(0x3b, 0x42, 0x52),
            selected: c(0x43, 0x4c, 0x5e),
            cursor: c(0x4c, 0x56, 0x6a),
            accent: c(0x88, 0xc0, 0xd0),
            text: c(0xec, 0xef, 0xf4),
            text_dim: c(0x9a, 0xa3, 0xb5),
            border: c(0x3b, 0x42, 0x52),
            danger: c(0xbf, 0x61, 0x6a),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::OneDark => Palette {
            dark: true,
            bg: c(0x21, 0x25, 0x2b),
            panel: c(0x28, 0x2c, 0x34),
            sidebar: c(0x21, 0x25, 0x2b),
            raised: c(0x2c, 0x31, 0x3a),
            hover: c(0x2f, 0x34, 0x3e),
            selected: c(0x3e, 0x44, 0x51),
            cursor: c(0x4b, 0x52, 0x63),
            accent: c(0x61, 0xaf, 0xef),
            text: c(0xd7, 0xda, 0xe0),
            text_dim: c(0x8b, 0x91, 0x9c),
            border: c(0x3a, 0x3f, 0x4b),
            danger: c(0xe0, 0x6c, 0x75),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::SolarizedLight => Palette {
            dark: false,
            bg: c(0xee, 0xe8, 0xd5),
            panel: c(0xfd, 0xf6, 0xe3),
            sidebar: c(0xee, 0xe8, 0xd5),
            raised: c(0xe9, 0xe2, 0xcb),
            hover: c(0xf3, 0xec, 0xd8),
            selected: c(0xd8, 0xe6, 0xe8),
            cursor: c(0xc6, 0xda, 0xdf),
            accent: c(0x26, 0x8b, 0xd2),
            text: c(0x07, 0x36, 0x42),
            text_dim: c(0x65, 0x7b, 0x83),
            border: c(0xdd, 0xd6, 0xc1),
            danger: c(0xdc, 0x32, 0x2f),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::RosePineDawn => Palette {
            dark: false,
            bg: c(0xf2, 0xe9, 0xe1),
            panel: c(0xfa, 0xf4, 0xed),
            sidebar: c(0xf2, 0xe9, 0xe1),
            raised: c(0xff, 0xfa, 0xf3),
            hover: c(0xf4, 0xed, 0xe8),
            selected: c(0xdf, 0xda, 0xd9),
            cursor: c(0xce, 0xca, 0xcd),
            accent: c(0x28, 0x69, 0x83),
            text: c(0x52, 0x4d, 0x72),
            text_dim: c(0x79, 0x75, 0x93),
            border: c(0xdf, 0xda, 0xd9),
            danger: c(0xb4, 0x63, 0x7a),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::Everforest => Palette {
            dark: true,
            bg: c(0x23, 0x2a, 0x2e),
            panel: c(0x2d, 0x35, 0x3b),
            sidebar: c(0x27, 0x2e, 0x33),
            raised: c(0x34, 0x3f, 0x44),
            hover: c(0x3d, 0x48, 0x4d),
            selected: c(0x47, 0x52, 0x58),
            cursor: c(0x4f, 0x58, 0x5e),
            accent: c(0xa7, 0xc0, 0x80),
            text: c(0xd3, 0xc6, 0xaa),
            text_dim: c(0x9d, 0xa9, 0xa0),
            border: c(0x3d, 0x48, 0x4d),
            danger: c(0xe6, 0x7e, 0x80),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Kanagawa => Palette {
            dark: true,
            bg: c(0x16, 0x16, 0x1d),
            panel: c(0x1f, 0x1f, 0x28),
            sidebar: c(0x1a, 0x1a, 0x22),
            raised: c(0x2a, 0x2a, 0x37),
            hover: c(0x2a, 0x2a, 0x37),
            selected: c(0x22, 0x32, 0x49),
            cursor: c(0x2d, 0x4f, 0x67),
            accent: c(0x7e, 0x9c, 0xd8),
            text: c(0xdc, 0xd7, 0xba),
            text_dim: c(0x93, 0x8f, 0x7f),
            border: c(0x2a, 0x2a, 0x37),
            danger: c(0xe8, 0x24, 0x24),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::GruvboxLight => Palette {
            dark: false,
            bg: c(0xf2, 0xe5, 0xbc),
            panel: c(0xfb, 0xf1, 0xc7),
            sidebar: c(0xf2, 0xe5, 0xbc),
            raised: c(0xeb, 0xdb, 0xb2),
            hover: c(0xf2, 0xe5, 0xbc),
            selected: c(0xe5, 0xd5, 0xab),
            cursor: c(0xd5, 0xc4, 0xa1),
            accent: c(0x07, 0x66, 0x78),
            text: c(0x3c, 0x38, 0x36),
            text_dim: c(0x7c, 0x6f, 0x64),
            border: c(0xe0, 0xd0, 0xa8),
            danger: c(0x9d, 0x00, 0x06),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::AyuLight => Palette {
            dark: false,
            bg: c(0xf0, 0xf1, 0xf2),
            panel: c(0xfc, 0xfc, 0xfc),
            sidebar: c(0xf3, 0xf4, 0xf5),
            raised: c(0xe9, 0xea, 0xeb),
            hover: c(0xee, 0xf0, 0xf2),
            selected: c(0xdc, 0xe9, 0xf5),
            cursor: c(0xc8, 0xdb, 0xf0),
            accent: c(0xff, 0x99, 0x40),
            text: c(0x52, 0x57, 0x5b),
            text_dim: c(0x8a, 0x91, 0x99),
            border: c(0xe1, 0xe3, 0xe6),
            danger: c(0xe6, 0x50, 0x50),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::RosePine => Palette {
            dark: true,
            bg: c(0x16, 0x14, 0x1f),
            panel: c(0x19, 0x17, 0x24),
            sidebar: c(0x1f, 0x1d, 0x2e),
            raised: c(0x26, 0x23, 0x3a),
            hover: c(0x2a, 0x27, 0x3f),
            selected: c(0x40, 0x3d, 0x52),
            cursor: c(0x52, 0x4f, 0x67),
            accent: c(0xc4, 0xa7, 0xe7),
            text: c(0xe0, 0xde, 0xf4),
            text_dim: c(0x90, 0x8c, 0xaa),
            border: c(0x26, 0x23, 0x3a),
            danger: c(0xeb, 0x6f, 0x92),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::SolarizedDark => Palette {
            dark: true,
            bg: c(0x00, 0x22, 0x2b),
            panel: c(0x00, 0x2b, 0x36),
            sidebar: c(0x00, 0x25, 0x2f),
            raised: c(0x07, 0x36, 0x42),
            hover: c(0x08, 0x3a, 0x47),
            selected: c(0x11, 0x47, 0x56),
            cursor: c(0x1a, 0x5a, 0x6b),
            accent: c(0x26, 0x8b, 0xd2),
            text: c(0xaa, 0xb5, 0xb5),
            text_dim: c(0x65, 0x7b, 0x83),
            border: c(0x07, 0x36, 0x42),
            danger: c(0xdc, 0x32, 0x2f),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::NightOwl => Palette {
            dark: true,
            bg: c(0x01, 0x0e, 0x1a),
            panel: c(0x01, 0x16, 0x27),
            sidebar: c(0x01, 0x11, 0x1d),
            raised: c(0x0b, 0x29, 0x42),
            hover: c(0x0e, 0x2a, 0x40),
            selected: c(0x1d, 0x3b, 0x53),
            cursor: c(0x23, 0x4d, 0x70),
            accent: c(0x82, 0xaa, 0xff),
            text: c(0xd6, 0xde, 0xeb),
            text_dim: c(0x7f, 0x97, 0xab),
            border: c(0x0b, 0x29, 0x42),
            danger: c(0xef, 0x53, 0x50),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::EverforestLight => Palette {
            dark: false,
            bg: c(0xf4, 0xf0, 0xd9),
            panel: c(0xfd, 0xf6, 0xe3),
            sidebar: c(0xf4, 0xf0, 0xd9),
            raised: c(0xef, 0xeb, 0xd4),
            hover: c(0xf4, 0xf0, 0xd9),
            selected: c(0xe2, 0xe6, 0xcc),
            cursor: c(0xd3, 0xdb, 0xbd),
            accent: c(0x35, 0xa7, 0x7c),
            text: c(0x49, 0x54, 0x5b),
            text_dim: c(0x84, 0x8f, 0x82),
            border: c(0xe6, 0xe2, 0xcc),
            danger: c(0xe9, 0x4f, 0x4d),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::TokyoNightDay => Palette {
            dark: false,
            bg: c(0xd5, 0xd6, 0xdb),
            panel: c(0xe1, 0xe2, 0xe7),
            sidebar: c(0xd8, 0xd9, 0xde),
            raised: c(0xe9, 0xe9, 0xed),
            hover: c(0xd9, 0xda, 0xe0),
            selected: c(0xc4, 0xcb, 0xe3),
            cursor: c(0xb7, 0xc1, 0xe3),
            accent: c(0x2e, 0x7d, 0xe9),
            text: c(0x26, 0x43, 0x85),
            text_dim: c(0x61, 0x72, 0xb0),
            border: c(0xc8, 0xc9, 0xd0),
            danger: c(0xc6, 0x43, 0x43),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::KanagawaLotus => Palette {
            dark: false,
            bg: c(0xe5, 0xdd, 0xb0),
            panel: c(0xf2, 0xec, 0xbc),
            sidebar: c(0xe5, 0xdd, 0xb0),
            raised: c(0xdc, 0xd5, 0xac),
            hover: c(0xeb, 0xe4, 0xb6),
            selected: c(0xcf, 0xd7, 0xcf),
            cursor: c(0xb5, 0xcb, 0xd2),
            accent: c(0x4d, 0x69, 0x9b),
            text: c(0x4b, 0x4b, 0x5a),
            text_dim: c(0x84, 0x83, 0x7a),
            border: c(0xdc, 0xd5, 0xac),
            danger: c(0xc8, 0x40, 0x53),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::CatppuccinFrappe => Palette {
            dark: true,
            bg: c(0x29, 0x2c, 0x3c),
            panel: c(0x30, 0x34, 0x46),
            sidebar: c(0x29, 0x2c, 0x3c),
            raised: c(0x3a, 0x3e, 0x52),
            hover: c(0x41, 0x45, 0x59),
            selected: c(0x4a, 0x4d, 0x6b),
            cursor: c(0x5a, 0x5c, 0x80),
            accent: c(0xca, 0x9e, 0xe6),
            text: c(0xc6, 0xd0, 0xf5),
            text_dim: c(0xa5, 0xad, 0xce),
            border: c(0x41, 0x45, 0x59),
            danger: c(0xe7, 0x82, 0x84),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::CatppuccinMacchiato => Palette {
            dark: true,
            bg: c(0x1e, 0x20, 0x30),
            panel: c(0x24, 0x27, 0x3a),
            sidebar: c(0x1e, 0x20, 0x30),
            raised: c(0x2e, 0x32, 0x47),
            hover: c(0x36, 0x3a, 0x4f),
            selected: c(0x45, 0x40, 0x5f),
            cursor: c(0x55, 0x4e, 0x78),
            accent: c(0xc6, 0xa0, 0xf6),
            text: c(0xca, 0xd3, 0xf5),
            text_dim: c(0xa5, 0xad, 0xcb),
            border: c(0x36, 0x3a, 0x4f),
            danger: c(0xed, 0x87, 0x96),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Monokai => Palette {
            dark: true,
            bg: c(0x1e, 0x1f, 0x1c),
            panel: c(0x27, 0x28, 0x22),
            sidebar: c(0x1e, 0x1f, 0x1c),
            raised: c(0x32, 0x33, 0x2c),
            hover: c(0x3e, 0x3d, 0x32),
            selected: c(0x49, 0x48, 0x3e),
            cursor: c(0x5b, 0x5a, 0x4c),
            accent: c(0xf9, 0x26, 0x72),
            text: c(0xf8, 0xf8, 0xf2),
            text_dim: c(0xa5, 0x9f, 0x85),
            border: c(0x3e, 0x3d, 0x32),
            danger: c(0xf9, 0x26, 0x72),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Palenight => Palette {
            dark: true,
            bg: c(0x22, 0x25, 0x33),
            panel: c(0x29, 0x2d, 0x3e),
            sidebar: c(0x24, 0x28, 0x38),
            raised: c(0x32, 0x37, 0x4d),
            hover: c(0x34, 0x32, 0x4a),
            selected: c(0x44, 0x42, 0x67),
            cursor: c(0x4e, 0x4c, 0x78),
            accent: c(0xc7, 0x92, 0xea),
            text: c(0xbf, 0xc7, 0xd5),
            text_dim: c(0x80, 0x87, 0xb5),
            border: c(0x32, 0x37, 0x4d),
            danger: c(0xff, 0x53, 0x70),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::AyuDark => Palette {
            dark: true,
            bg: c(0x0b, 0x0e, 0x14),
            panel: c(0x0d, 0x10, 0x17),
            sidebar: c(0x0b, 0x0e, 0x14),
            raised: c(0x13, 0x17, 0x21),
            hover: c(0x16, 0x1b, 0x26),
            selected: c(0x1b, 0x2a, 0x3e),
            cursor: c(0x23, 0x39, 0x5b),
            accent: c(0xe6, 0xb4, 0x50),
            text: c(0xbf, 0xbd, 0xb6),
            text_dim: c(0x6c, 0x73, 0x80),
            border: c(0x1b, 0x1f, 0x29),
            danger: c(0xd9, 0x57, 0x57),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::AyuMirage => Palette {
            dark: true,
            bg: c(0x1f, 0x24, 0x30),
            panel: c(0x24, 0x29, 0x36),
            sidebar: c(0x1f, 0x24, 0x30),
            raised: c(0x2a, 0x30, 0x40),
            hover: c(0x2d, 0x33, 0x43),
            selected: c(0x33, 0x41, 0x5e),
            cursor: c(0x3f, 0x50, 0x70),
            accent: c(0xff, 0xcc, 0x66),
            text: c(0xcc, 0xca, 0xc2),
            text_dim: c(0x8a, 0x91, 0x99),
            border: c(0x2d, 0x33, 0x43),
            danger: c(0xf2, 0x87, 0x79),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::RosePineMoon => Palette {
            dark: true,
            bg: c(0x1f, 0x1d, 0x30),
            panel: c(0x23, 0x21, 0x36),
            sidebar: c(0x2a, 0x27, 0x3f),
            raised: c(0x39, 0x35, 0x52),
            hover: c(0x2f, 0x2c, 0x47),
            selected: c(0x44, 0x41, 0x5a),
            cursor: c(0x56, 0x52, 0x6e),
            accent: c(0xc4, 0xa7, 0xe7),
            text: c(0xe0, 0xde, 0xf4),
            text_dim: c(0x90, 0x8c, 0xaa),
            border: c(0x39, 0x35, 0x52),
            danger: c(0xeb, 0x6f, 0x92),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::TokyoNightStorm => Palette {
            dark: true,
            bg: c(0x1f, 0x23, 0x35),
            panel: c(0x24, 0x28, 0x3b),
            sidebar: c(0x1f, 0x23, 0x35),
            raised: c(0x29, 0x2e, 0x42),
            hover: c(0x2f, 0x34, 0x4d),
            selected: c(0x2e, 0x3c, 0x64),
            cursor: c(0x3d, 0x4f, 0x80),
            accent: c(0x7a, 0xa2, 0xf7),
            text: c(0xc0, 0xca, 0xf5),
            text_dim: c(0x78, 0x7c, 0x99),
            border: c(0x29, 0x2e, 0x42),
            danger: c(0xf7, 0x76, 0x8e),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::KanagawaDragon => Palette {
            dark: true,
            bg: c(0x12, 0x12, 0x0f),
            panel: c(0x18, 0x16, 0x16),
            sidebar: c(0x12, 0x12, 0x0f),
            raised: c(0x28, 0x27, 0x27),
            hover: c(0x28, 0x27, 0x27),
            selected: c(0x39, 0x38, 0x36),
            cursor: c(0x4a, 0x47, 0x44),
            accent: c(0x8b, 0xa4, 0xb0),
            text: c(0xc5, 0xc9, 0xc5),
            text_dim: c(0xa6, 0xa6, 0x9c),
            border: c(0x28, 0x27, 0x27),
            danger: c(0xc4, 0x74, 0x6e),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Nightfox => Palette {
            dark: true,
            bg: c(0x13, 0x1a, 0x24),
            panel: c(0x19, 0x23, 0x30),
            sidebar: c(0x13, 0x1a, 0x24),
            raised: c(0x21, 0x2e, 0x3f),
            hover: c(0x21, 0x2e, 0x3f),
            selected: c(0x2b, 0x3b, 0x51),
            cursor: c(0x3c, 0x53, 0x72),
            accent: c(0x71, 0x9c, 0xd6),
            text: c(0xcd, 0xce, 0xcf),
            text_dim: c(0x8d, 0x9c, 0xad),
            border: c(0x29, 0x39, 0x4f),
            danger: c(0xc9, 0x4f, 0x6d),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::FlexokiDark => Palette {
            dark: true,
            bg: c(0x10, 0x0f, 0x0f),
            panel: c(0x1c, 0x1b, 0x1a),
            sidebar: c(0x10, 0x0f, 0x0f),
            raised: c(0x28, 0x27, 0x26),
            hover: c(0x28, 0x27, 0x26),
            selected: c(0x34, 0x33, 0x31),
            cursor: c(0x40, 0x3e, 0x3c),
            accent: c(0xda, 0x70, 0x2c),
            text: c(0xce, 0xcd, 0xc3),
            text_dim: c(0x87, 0x85, 0x80),
            border: c(0x28, 0x27, 0x26),
            danger: c(0xd1, 0x4d, 0x41),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::OneLight => Palette {
            dark: false,
            bg: c(0xf0, 0xf0, 0xf0),
            panel: c(0xfa, 0xfa, 0xfa),
            sidebar: c(0xf0, 0xf0, 0xf1),
            raised: c(0xea, 0xea, 0xeb),
            hover: c(0xef, 0xef, 0xf0),
            selected: c(0xdb, 0xe5, 0xfb),
            cursor: c(0xc7, 0xd6, 0xf7),
            accent: c(0x40, 0x78, 0xf2),
            text: c(0x38, 0x3a, 0x42),
            text_dim: c(0x69, 0x6c, 0x77),
            border: c(0xe5, 0xe5, 0xe6),
            danger: c(0xe4, 0x56, 0x49),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::LightOwl => Palette {
            dark: false,
            bg: c(0xf0, 0xf0, 0xf0),
            panel: c(0xfb, 0xfb, 0xfb),
            sidebar: c(0xf0, 0xf0, 0xf0),
            raised: c(0xe9, 0xe9, 0xea),
            hover: c(0xf0, 0xf0, 0xf1),
            selected: c(0xe0, 0xe7, 0xf5),
            cursor: c(0xc9, 0xd6, 0xf0),
            accent: c(0x2a, 0xa2, 0x98),
            text: c(0x40, 0x3f, 0x53),
            text_dim: c(0x6e, 0x6c, 0x80),
            border: c(0xe0, 0xe0, 0xe0),
            danger: c(0xde, 0x3d, 0x3b),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::Dayfox => Palette {
            dark: false,
            bg: c(0xe4, 0xdc, 0xd4),
            panel: c(0xf6, 0xf2, 0xee),
            sidebar: c(0xe4, 0xdc, 0xd4),
            raised: c(0xeb, 0xe5, 0xdf),
            hover: c(0xef, 0xe8, 0xe1),
            selected: c(0xe7, 0xd2, 0xbe),
            cursor: c(0xa4, 0xc1, 0xc2),
            accent: c(0x28, 0x48, 0xa9),
            text: c(0x3d, 0x2b, 0x5a),
            text_dim: c(0x83, 0x7a, 0x72),
            border: c(0xd3, 0xc7, 0xbb),
            danger: c(0xa5, 0x22, 0x2f),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::FlexokiLight => Palette {
            dark: false,
            bg: c(0xf2, 0xf0, 0xe5),
            panel: c(0xff, 0xfc, 0xf0),
            sidebar: c(0xf2, 0xf0, 0xe5),
            raised: c(0xe6, 0xe4, 0xd9),
            hover: c(0xf2, 0xf0, 0xe5),
            selected: c(0xda, 0xd8, 0xce),
            cursor: c(0xce, 0xcd, 0xc3),
            accent: c(0x20, 0x5e, 0xa6),
            text: c(0x10, 0x0f, 0x0f),
            text_dim: c(0x6f, 0x6e, 0x69),
            border: c(0xe6, 0xe4, 0xd9),
            danger: c(0xaf, 0x30, 0x29),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::GitHubDark => Palette {
            dark: true,
            bg: c(0x01, 0x04, 0x09),
            panel: c(0x0d, 0x11, 0x17),
            sidebar: c(0x01, 0x04, 0x09),
            raised: c(0x16, 0x1b, 0x22),
            hover: c(0x16, 0x1b, 0x22),
            selected: c(0x16, 0x33, 0x56),
            cursor: c(0x1f, 0x4a, 0x85),
            accent: c(0x2f, 0x81, 0xf7),
            text: c(0xe6, 0xed, 0xf3),
            text_dim: c(0x8d, 0x96, 0xa0),
            border: c(0x30, 0x36, 0x3d),
            danger: c(0xf8, 0x51, 0x49),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::GitHubDimmed => Palette {
            dark: true,
            bg: c(0x1c, 0x21, 0x28),
            panel: c(0x22, 0x27, 0x2e),
            sidebar: c(0x1c, 0x21, 0x28),
            raised: c(0x2d, 0x33, 0x3b),
            hover: c(0x2a, 0x30, 0x38),
            selected: c(0x2a, 0x43, 0x65),
            cursor: c(0x30, 0x55, 0x8a),
            accent: c(0x53, 0x9b, 0xf5),
            text: c(0xad, 0xba, 0xc7),
            text_dim: c(0x76, 0x83, 0x90),
            border: c(0x44, 0x4c, 0x56),
            danger: c(0xe5, 0x53, 0x4b),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Zenburn => Palette {
            dark: true,
            bg: c(0x38, 0x38, 0x38),
            panel: c(0x3f, 0x3f, 0x3f),
            sidebar: c(0x38, 0x38, 0x38),
            raised: c(0x4f, 0x4f, 0x4f),
            hover: c(0x4a, 0x4a, 0x4a),
            selected: c(0x5b, 0x62, 0x5e),
            cursor: c(0x6f, 0x7f, 0x78),
            accent: c(0x8c, 0xd0, 0xd3),
            text: c(0xdc, 0xdc, 0xcc),
            text_dim: c(0x9f, 0xaf, 0xaf),
            border: c(0x4f, 0x4f, 0x4f),
            danger: c(0xcc, 0x93, 0x93),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::TomorrowNight => Palette {
            dark: true,
            bg: c(0x16, 0x17, 0x19),
            panel: c(0x1d, 0x1f, 0x21),
            sidebar: c(0x16, 0x17, 0x19),
            raised: c(0x28, 0x2a, 0x2e),
            hover: c(0x2c, 0x2e, 0x32),
            selected: c(0x37, 0x3b, 0x41),
            cursor: c(0x4b, 0x50, 0x58),
            accent: c(0x81, 0xa2, 0xbe),
            text: c(0xc5, 0xc8, 0xc6),
            text_dim: c(0x96, 0x98, 0x96),
            border: c(0x28, 0x2a, 0x2e),
            danger: c(0xcc, 0x66, 0x66),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::OceanicNext => Palette {
            dark: true,
            bg: c(0x16, 0x23, 0x2a),
            panel: c(0x1b, 0x2b, 0x34),
            sidebar: c(0x16, 0x23, 0x2a),
            raised: c(0x34, 0x3d, 0x46),
            hover: c(0x2a, 0x3a, 0x44),
            selected: c(0x4f, 0x5b, 0x66),
            cursor: c(0x65, 0x73, 0x7e),
            accent: c(0x66, 0x99, 0xcc),
            text: c(0xd8, 0xde, 0xe9),
            text_dim: c(0xa7, 0xad, 0xba),
            border: c(0x34, 0x3d, 0x46),
            danger: c(0xec, 0x5f, 0x67),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Horizon => Palette {
            dark: true,
            bg: c(0x16, 0x16, 0x1c),
            panel: c(0x1c, 0x1e, 0x26),
            sidebar: c(0x16, 0x16, 0x1c),
            raised: c(0x23, 0x25, 0x30),
            hover: c(0x2a, 0x2c, 0x38),
            selected: c(0x3a, 0x3d, 0x50),
            cursor: c(0x4c, 0x4f, 0x66),
            accent: c(0xe9, 0x56, 0x78),
            text: c(0xd5, 0xd8, 0xda),
            text_dim: c(0x90, 0x93, 0xad),
            border: c(0x2e, 0x30, 0x3e),
            danger: c(0xf4, 0x3e, 0x5c),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Cobalt2 => Palette {
            dark: true,
            bg: c(0x15, 0x23, 0x2d),
            panel: c(0x19, 0x35, 0x49),
            sidebar: c(0x15, 0x23, 0x2d),
            raised: c(0x1f, 0x46, 0x62),
            hover: c(0x1e, 0x40, 0x58),
            selected: c(0x00, 0x50, 0xa4),
            cursor: c(0x0d, 0x6a, 0xd1),
            accent: c(0xff, 0xc6, 0x00),
            text: c(0xff, 0xff, 0xff),
            text_dim: c(0x9f, 0xb3, 0xc8),
            border: c(0x0d, 0x3a, 0x58),
            danger: c(0xff, 0x62, 0x8c),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Synthwave => Palette {
            dark: true,
            bg: c(0x1e, 0x1a, 0x2a),
            panel: c(0x26, 0x23, 0x35),
            sidebar: c(0x24, 0x1b, 0x2f),
            raised: c(0x34, 0x29, 0x4f),
            hover: c(0x2f, 0x2a, 0x45),
            selected: c(0x46, 0x34, 0x65),
            cursor: c(0x5a, 0x43, 0x85),
            accent: c(0xff, 0x7e, 0xdb),
            text: c(0xf0, 0xef, 0xf1),
            text_dim: c(0x84, 0x8b, 0xbd),
            border: c(0x34, 0x29, 0x4f),
            danger: c(0xfe, 0x44, 0x50),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Moonlight => Palette {
            dark: true,
            bg: c(0x1e, 0x20, 0x30),
            panel: c(0x22, 0x24, 0x36),
            sidebar: c(0x1e, 0x20, 0x30),
            raised: c(0x2f, 0x33, 0x4d),
            hover: c(0x2a, 0x2d, 0x44),
            selected: c(0x2d, 0x3f, 0x76),
            cursor: c(0x3e, 0x52, 0xa0),
            accent: c(0x82, 0xaa, 0xff),
            text: c(0xc8, 0xd3, 0xf5),
            text_dim: c(0x82, 0x8b, 0xb8),
            border: c(0x2f, 0x33, 0x4d),
            danger: c(0xff, 0x75, 0x7f),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Poimandres => Palette {
            dark: true,
            bg: c(0x17, 0x19, 0x22),
            panel: c(0x1b, 0x1e, 0x28),
            sidebar: c(0x17, 0x19, 0x22),
            raised: c(0x25, 0x2b, 0x37),
            hover: c(0x22, 0x26, 0x32),
            selected: c(0x30, 0x33, 0x40),
            cursor: c(0x40, 0x43, 0x50),
            accent: c(0x5d, 0xe4, 0xc7),
            text: c(0xe4, 0xf0, 0xfb),
            text_dim: c(0xa6, 0xac, 0xcd),
            border: c(0x25, 0x2b, 0x37),
            danger: c(0xd0, 0x67, 0x9d),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Oxocarbon => Palette {
            dark: true,
            bg: c(0x0f, 0x0f, 0x0f),
            panel: c(0x16, 0x16, 0x16),
            sidebar: c(0x12, 0x12, 0x12),
            raised: c(0x26, 0x26, 0x26),
            hover: c(0x20, 0x20, 0x20),
            selected: c(0x39, 0x39, 0x39),
            cursor: c(0x52, 0x52, 0x52),
            accent: c(0x33, 0xb1, 0xff),
            text: c(0xf2, 0xf4, 0xf8),
            text_dim: c(0x8d, 0x8d, 0x8d),
            border: c(0x26, 0x26, 0x26),
            danger: c(0xee, 0x53, 0x96),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::ModusVivendi => Palette {
            dark: true,
            bg: c(0x1e, 0x1e, 0x1e),
            panel: c(0x00, 0x00, 0x00),
            sidebar: c(0x1e, 0x1e, 0x1e),
            raised: c(0x30, 0x30, 0x30),
            hover: c(0x1a, 0x1a, 0x1a),
            selected: c(0x2f, 0x38, 0x49),
            cursor: c(0x3c, 0x4a, 0x66),
            accent: c(0x2f, 0xaf, 0xff),
            text: c(0xff, 0xff, 0xff),
            text_dim: c(0x98, 0x98, 0x98),
            border: c(0x64, 0x64, 0x64),
            danger: c(0xff, 0x5f, 0x59),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Iceberg => Palette {
            dark: true,
            bg: c(0x0f, 0x11, 0x17),
            panel: c(0x16, 0x18, 0x21),
            sidebar: c(0x0f, 0x11, 0x17),
            raised: c(0x1e, 0x21, 0x32),
            hover: c(0x1b, 0x1e, 0x2b),
            selected: c(0x27, 0x2c, 0x42),
            cursor: c(0x3d, 0x42, 0x5b),
            accent: c(0x84, 0xa0, 0xc6),
            text: c(0xc6, 0xc8, 0xd1),
            text_dim: c(0x6b, 0x70, 0x89),
            border: c(0x1e, 0x21, 0x32),
            danger: c(0xe2, 0x78, 0x78),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::GitHubLight => Palette {
            dark: false,
            bg: c(0xf6, 0xf8, 0xfa),
            panel: c(0xff, 0xff, 0xff),
            sidebar: c(0xf6, 0xf8, 0xfa),
            raised: c(0xef, 0xf2, 0xf5),
            hover: c(0xf3, 0xf4, 0xf6),
            selected: c(0xdd, 0xf4, 0xff),
            cursor: c(0xb6, 0xe3, 0xff),
            accent: c(0x09, 0x69, 0xda),
            text: c(0x1f, 0x23, 0x28),
            text_dim: c(0x65, 0x6d, 0x76),
            border: c(0xd0, 0xd7, 0xde),
            danger: c(0xcf, 0x22, 0x2e),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::Tomorrow => Palette {
            dark: false,
            bg: c(0xf2, 0xf2, 0xf2),
            panel: c(0xff, 0xff, 0xff),
            sidebar: c(0xf2, 0xf2, 0xf2),
            raised: c(0xeb, 0xeb, 0xeb),
            hover: c(0xf0, 0xf0, 0xf0),
            selected: c(0xd6, 0xd6, 0xd6),
            cursor: c(0xc5, 0xc5, 0xc5),
            accent: c(0x42, 0x71, 0xae),
            text: c(0x4d, 0x4d, 0x4c),
            text_dim: c(0x8e, 0x90, 0x8c),
            border: c(0xe0, 0xe0, 0xe0),
            danger: c(0xc8, 0x28, 0x29),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::IcebergLight => Palette {
            dark: false,
            bg: c(0xdc, 0xdf, 0xe7),
            panel: c(0xe8, 0xe9, 0xec),
            sidebar: c(0xdc, 0xdf, 0xe7),
            raised: c(0xd2, 0xd4, 0xde),
            hover: c(0xdf, 0xe1, 0xe8),
            selected: c(0xc9, 0xcd, 0xd7),
            cursor: c(0xb8, 0xbd, 0xcc),
            accent: c(0x2d, 0x53, 0x9e),
            text: c(0x33, 0x37, 0x4c),
            text_dim: c(0x6f, 0x75, 0x91),
            border: c(0xca, 0xd0, 0xde),
            danger: c(0xcc, 0x51, 0x7a),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::ModusOperandi => Palette {
            dark: false,
            bg: c(0xf2, 0xf2, 0xf2),
            panel: c(0xff, 0xff, 0xff),
            sidebar: c(0xf2, 0xf2, 0xf2),
            raised: c(0xe6, 0xe6, 0xe6),
            hover: c(0xf5, 0xf5, 0xf5),
            selected: c(0xda, 0xe5, 0xec),
            cursor: c(0xbc, 0xbc, 0xbc),
            accent: c(0x00, 0x31, 0xa9),
            text: c(0x00, 0x00, 0x00),
            text_dim: c(0x59, 0x59, 0x59),
            border: c(0x9f, 0x9f, 0x9f),
            danger: c(0xa6, 0x00, 0x00),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::SelenizedLight => Palette {
            dark: false,
            bg: c(0xec, 0xe3, 0xcc),
            panel: c(0xfb, 0xf3, 0xdb),
            sidebar: c(0xec, 0xe3, 0xcc),
            raised: c(0xe3, 0xd9, 0xc0),
            hover: c(0xf3, 0xea, 0xd2),
            selected: c(0xd5, 0xcd, 0xb6),
            cursor: c(0xc4, 0xbc, 0xa5),
            accent: c(0x00, 0x72, 0xd4),
            text: c(0x3a, 0x4d, 0x53),
            text_dim: c(0x53, 0x67, 0x6d),
            border: c(0xd5, 0xcd, 0xb6),
            danger: c(0xd2, 0x21, 0x2d),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::QuietLight => Palette {
            dark: false,
            bg: c(0xec, 0xec, 0xec),
            panel: c(0xf5, 0xf5, 0xf5),
            sidebar: c(0xf2, 0xf2, 0xf2),
            raised: c(0xe7, 0xe7, 0xe7),
            hover: c(0xed, 0xed, 0xed),
            selected: c(0xc4, 0xd9, 0xb1),
            cursor: c(0xa8, 0xc4, 0x8f),
            accent: c(0x70, 0x56, 0x97),
            text: c(0x33, 0x33, 0x33),
            text_dim: c(0x77, 0x77, 0x77),
            border: c(0xdc, 0xdc, 0xdc),
            danger: c(0xaa, 0x37, 0x31),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::PaperColor => Palette {
            dark: false,
            bg: c(0xe4, 0xe4, 0xe4),
            panel: c(0xee, 0xee, 0xee),
            sidebar: c(0xe4, 0xe4, 0xe4),
            raised: c(0xda, 0xda, 0xda),
            hover: c(0xe8, 0xe8, 0xe8),
            selected: c(0xd0, 0xd0, 0xd0),
            cursor: c(0xbc, 0xbc, 0xbc),
            accent: c(0x00, 0x5f, 0xaf),
            text: c(0x44, 0x44, 0x44),
            text_dim: c(0x87, 0x87, 0x87),
            border: c(0xd0, 0xd0, 0xd0),
            danger: c(0xaf, 0x00, 0x00),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::DarkModern => Palette {
            dark: true,
            bg: c(0x18, 0x18, 0x18),
            panel: c(0x1f, 0x1f, 0x1f),
            sidebar: c(0x18, 0x18, 0x18),
            raised: c(0x20, 0x20, 0x20),
            hover: c(0x2a, 0x2d, 0x2e),
            selected: c(0x04, 0x39, 0x5e),
            cursor: c(0x09, 0x47, 0x71),
            accent: c(0x00, 0x78, 0xd4),
            text: c(0xcc, 0xcc, 0xcc),
            text_dim: c(0x9d, 0x9d, 0x9d),
            border: c(0x2b, 0x2b, 0x2b),
            danger: c(0xf8, 0x51, 0x49),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Darcula => Palette {
            dark: true,
            bg: c(0x3c, 0x3f, 0x41),
            panel: c(0x2b, 0x2b, 0x2b),
            sidebar: c(0x3c, 0x3f, 0x41),
            raised: c(0x3c, 0x3f, 0x41),
            hover: c(0x4b, 0x4d, 0x4f),
            selected: c(0x0d, 0x29, 0x3e),
            cursor: c(0x2f, 0x65, 0xca),
            accent: c(0x58, 0x9d, 0xf6),
            text: c(0xbb, 0xbb, 0xbb),
            text_dim: c(0x80, 0x80, 0x80),
            border: c(0x32, 0x32, 0x32),
            danger: c(0xff, 0x6b, 0x68),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::XcodeDark => Palette {
            dark: true,
            bg: c(0x2a, 0x2c, 0x2f),
            panel: c(0x1f, 0x1f, 0x24),
            sidebar: c(0x2a, 0x2c, 0x2f),
            raised: c(0x2b, 0x2b, 0x30),
            hover: c(0x2a, 0x2b, 0x30),
            selected: c(0x3b, 0x3f, 0x4a),
            cursor: c(0x30, 0x71, 0xdb),
            accent: c(0x0a, 0x84, 0xff),
            text: c(0xdf, 0xdf, 0xe0),
            text_dim: c(0x8e, 0x8e, 0x93),
            border: c(0x38, 0x38, 0x3d),
            danger: c(0xff, 0x45, 0x3a),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::MaterialDarker => Palette {
            dark: true,
            bg: c(0x1a, 0x1a, 0x1a),
            panel: c(0x21, 0x21, 0x21),
            sidebar: c(0x1a, 0x1a, 0x1a),
            raised: c(0x29, 0x29, 0x29),
            hover: c(0x2a, 0x2a, 0x2a),
            selected: c(0x32, 0x32, 0x32),
            cursor: c(0x40, 0x40, 0x40),
            accent: c(0xff, 0x98, 0x00),
            text: c(0xb0, 0xbe, 0xc5),
            text_dim: c(0x72, 0x72, 0x72),
            border: c(0x29, 0x29, 0x29),
            danger: c(0xf0, 0x71, 0x78),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::DoomOne => Palette {
            dark: true,
            bg: c(0x21, 0x24, 0x2b),
            panel: c(0x28, 0x2c, 0x34),
            sidebar: c(0x21, 0x24, 0x2b),
            raised: c(0x21, 0x24, 0x2b),
            hover: c(0x2c, 0x31, 0x3a),
            selected: c(0x42, 0x45, 0x4b),
            cursor: c(0x22, 0x57, 0xa0),
            accent: c(0x51, 0xaf, 0xef),
            text: c(0xbb, 0xc2, 0xcf),
            text_dim: c(0x9c, 0xa0, 0xa4),
            border: c(0x3f, 0x44, 0x4a),
            danger: c(0xff, 0x6c, 0x6b),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Sonokai => Palette {
            dark: true,
            bg: c(0x22, 0x23, 0x27),
            panel: c(0x2c, 0x2e, 0x34),
            sidebar: c(0x22, 0x23, 0x27),
            raised: c(0x36, 0x39, 0x44),
            hover: c(0x33, 0x35, 0x3f),
            selected: c(0x3b, 0x3e, 0x48),
            cursor: c(0x35, 0x41, 0x57),
            accent: c(0x76, 0xcc, 0xe0),
            text: c(0xe2, 0xe2, 0xe3),
            text_dim: c(0x7f, 0x84, 0x90),
            border: c(0x41, 0x45, 0x50),
            danger: c(0xfc, 0x5d, 0x7c),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::GruvboxMaterial => Palette {
            dark: true,
            bg: c(0x1b, 0x1b, 0x1b),
            panel: c(0x28, 0x28, 0x28),
            sidebar: c(0x1b, 0x1b, 0x1b),
            raised: c(0x32, 0x30, 0x2f),
            hover: c(0x32, 0x30, 0x2f),
            selected: c(0x45, 0x40, 0x3d),
            cursor: c(0x5a, 0x52, 0x4c),
            accent: c(0xa9, 0xb6, 0x65),
            text: c(0xd4, 0xbe, 0x98),
            text_dim: c(0xa8, 0x99, 0x84),
            border: c(0x45, 0x40, 0x3d),
            danger: c(0xea, 0x69, 0x62),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Vesper => Palette {
            dark: true,
            bg: c(0x10, 0x10, 0x10),
            panel: c(0x10, 0x10, 0x10),
            sidebar: c(0x10, 0x10, 0x10),
            raised: c(0x1c, 0x1c, 0x1c),
            hover: c(0x28, 0x28, 0x28),
            selected: c(0x23, 0x23, 0x23),
            cursor: c(0x3a, 0x3a, 0x3a),
            accent: c(0xff, 0xc7, 0x99),
            text: c(0xff, 0xff, 0xff),
            text_dim: c(0xa0, 0xa0, 0xa0),
            border: c(0x28, 0x28, 0x28),
            danger: c(0xff, 0x80, 0x80),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Andromeda => Palette {
            dark: true,
            bg: c(0x20, 0x23, 0x2a),
            panel: c(0x23, 0x26, 0x2e),
            sidebar: c(0x20, 0x23, 0x2a),
            raised: c(0x2b, 0x30, 0x3b),
            hover: c(0x28, 0x2b, 0x35),
            selected: c(0x3d, 0x43, 0x52),
            cursor: c(0x4b, 0x54, 0x68),
            accent: c(0x00, 0xe8, 0xc6),
            text: c(0xd5, 0xce, 0xd9),
            text_dim: c(0xa0, 0xa1, 0xa7),
            border: c(0x1b, 0x1d, 0x23),
            danger: c(0xfc, 0x64, 0x4d),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::ShadesOfPurple => Palette {
            dark: true,
            bg: c(0x1e, 0x1e, 0x3f),
            panel: c(0x2d, 0x2b, 0x55),
            sidebar: c(0x22, 0x22, 0x44),
            raised: c(0x1e, 0x1e, 0x3f),
            hover: c(0x37, 0x35, 0x6b),
            selected: c(0x4d, 0x4a, 0x8c),
            cursor: c(0x74, 0x48, 0xb0),
            accent: c(0xfa, 0xd0, 0x00),
            text: c(0xff, 0xff, 0xff),
            text_dim: c(0xa5, 0x99, 0xe9),
            border: c(0x25, 0x25, 0x4b),
            danger: c(0xec, 0x3a, 0x37),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Carbonfox => Palette {
            dark: true,
            bg: c(0x0c, 0x0c, 0x0c),
            panel: c(0x16, 0x16, 0x16),
            sidebar: c(0x0c, 0x0c, 0x0c),
            raised: c(0x25, 0x25, 0x25),
            hover: c(0x25, 0x25, 0x25),
            selected: c(0x2a, 0x2a, 0x2a),
            cursor: c(0x52, 0x52, 0x53),
            accent: c(0x78, 0xa9, 0xff),
            text: c(0xf2, 0xf4, 0xf8),
            text_dim: c(0x7b, 0x7c, 0x7e),
            border: c(0x35, 0x35, 0x35),
            danger: c(0xee, 0x53, 0x96),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Duskfox => Palette {
            dark: true,
            bg: c(0x19, 0x17, 0x26),
            panel: c(0x23, 0x21, 0x36),
            sidebar: c(0x19, 0x17, 0x26),
            raised: c(0x2d, 0x2a, 0x45),
            hover: c(0x2d, 0x2a, 0x45),
            selected: c(0x43, 0x3c, 0x59),
            cursor: c(0x63, 0x57, 0x7d),
            accent: c(0x56, 0x9f, 0xba),
            text: c(0xe0, 0xde, 0xf4),
            text_dim: c(0x81, 0x7c, 0x9c),
            border: c(0x37, 0x33, 0x54),
            danger: c(0xeb, 0x6f, 0x92),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::VitesseDark => Palette {
            dark: true,
            bg: c(0x12, 0x12, 0x12),
            panel: c(0x12, 0x12, 0x12),
            sidebar: c(0x12, 0x12, 0x12),
            raised: c(0x18, 0x18, 0x18),
            hover: c(0x18, 0x18, 0x18),
            selected: c(0x27, 0x27, 0x27),
            cursor: c(0x3b, 0x3b, 0x3b),
            accent: c(0x4d, 0x93, 0x75),
            text: c(0xce, 0xca, 0xbe),
            text_dim: c(0x85, 0x84, 0x80),
            border: c(0x19, 0x19, 0x19),
            danger: c(0xcb, 0x76, 0x76),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::LightModern => Palette {
            dark: false,
            bg: c(0xf8, 0xf8, 0xf8),
            panel: c(0xff, 0xff, 0xff),
            sidebar: c(0xf8, 0xf8, 0xf8),
            raised: c(0xf8, 0xf8, 0xf8),
            hover: c(0xf2, 0xf2, 0xf2),
            selected: c(0xe8, 0xe8, 0xe8),
            cursor: c(0xcc, 0xe6, 0xff),
            accent: c(0x00, 0x5f, 0xb8),
            text: c(0x3b, 0x3b, 0x3b),
            text_dim: c(0x76, 0x76, 0x76),
            border: c(0xe5, 0xe5, 0xe5),
            danger: c(0xf8, 0x51, 0x49),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::IntelliJLight => Palette {
            dark: false,
            bg: c(0xf7, 0xf8, 0xfa),
            panel: c(0xff, 0xff, 0xff),
            sidebar: c(0xf7, 0xf8, 0xfa),
            raised: c(0xff, 0xff, 0xff),
            hover: c(0xeb, 0xec, 0xf0),
            selected: c(0xdf, 0xe1, 0xe5),
            cursor: c(0xd4, 0xe2, 0xff),
            accent: c(0x35, 0x74, 0xf0),
            text: c(0x08, 0x08, 0x08),
            text_dim: c(0x6c, 0x70, 0x7e),
            border: c(0xeb, 0xec, 0xf0),
            danger: c(0xdb, 0x3b, 0x4b),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::XcodeLight => Palette {
            dark: false,
            bg: c(0xf2, 0xf2, 0xf7),
            panel: c(0xff, 0xff, 0xff),
            sidebar: c(0xf2, 0xf2, 0xf7),
            raised: c(0xf5, 0xf5, 0xf7),
            hover: c(0xec, 0xec, 0xf0),
            selected: c(0xe8, 0xf2, 0xff),
            cursor: c(0xa4, 0xcd, 0xff),
            accent: c(0x00, 0x7a, 0xff),
            text: c(0x26, 0x26, 0x26),
            text_dim: c(0x8e, 0x8e, 0x93),
            border: c(0xdc, 0xdc, 0xe0),
            danger: c(0xff, 0x3b, 0x30),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        // Its darkest text, and its comment grey a shade deeper for dim text, to read on its base.
        ThemeName::Dawnfox => Palette {
            dark: false,
            bg: c(0xeb, 0xe5, 0xdf),
            panel: c(0xfa, 0xf4, 0xed),
            sidebar: c(0xeb, 0xe5, 0xdf),
            raised: c(0xeb, 0xe0, 0xdf),
            hover: c(0xeb, 0xdf, 0xe4),
            selected: c(0xd0, 0xd8, 0xd8),
            cursor: c(0xb8, 0xce, 0xce),
            accent: c(0x28, 0x69, 0x83),
            text: c(0x4c, 0x47, 0x69),
            text_dim: c(0x8a, 0x85, 0xa0),
            border: c(0xbd, 0xbf, 0xc9),
            danger: c(0xb4, 0x63, 0x7a),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::VitesseLight => Palette {
            dark: false,
            bg: c(0xf7, 0xf7, 0xf7),
            panel: c(0xff, 0xff, 0xff),
            sidebar: c(0xf7, 0xf7, 0xf7),
            raised: c(0xf7, 0xf7, 0xf7),
            hover: c(0xf0, 0xf0, 0xf0),
            selected: c(0xea, 0xea, 0xea),
            cursor: c(0xd5, 0xd5, 0xd5),
            accent: c(0x1c, 0x6b, 0x48),
            text: c(0x39, 0x3a, 0x34),
            text_dim: c(0x8f, 0x8f, 0x8c),
            border: c(0xf0, 0xf0, 0xf0),
            danger: c(0xab, 0x59, 0x59),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::GruvboxMaterialLight => Palette {
            dark: false,
            bg: c(0xf2, 0xe5, 0xbc),
            panel: c(0xfb, 0xf1, 0xc7),
            sidebar: c(0xf2, 0xe5, 0xbc),
            raised: c(0xf4, 0xe8, 0xbe),
            hover: c(0xee, 0xe0, 0xb7),
            selected: c(0xe5, 0xd5, 0xad),
            cursor: c(0xdd, 0xcc, 0xab),
            accent: c(0x6c, 0x78, 0x2e),
            text: c(0x65, 0x47, 0x35),
            text_dim: c(0x7c, 0x6f, 0x64),
            border: c(0xe5, 0xd5, 0xad),
            danger: c(0xc1, 0x4a, 0x4a),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::Alabaster => Palette {
            dark: false,
            bg: c(0xee, 0xee, 0xee),
            panel: c(0xf7, 0xf7, 0xf7),
            sidebar: c(0xee, 0xee, 0xee),
            raised: c(0xff, 0xff, 0xff),
            hover: c(0xec, 0xec, 0xec),
            selected: c(0xe4, 0xe4, 0xe4),
            cursor: c(0xbf, 0xdb, 0xfe),
            accent: c(0x32, 0x5c, 0xc0),
            text: c(0x00, 0x00, 0x00),
            text_dim: c(0x77, 0x77, 0x77),
            border: c(0xdd, 0xdd, 0xdd),
            danger: c(0xaa, 0x37, 0x31),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::MonokaiPro => Palette {
            dark: true,
            bg: c(0x22, 0x1f, 0x22),
            panel: c(0x2d, 0x2a, 0x2e),
            sidebar: c(0x22, 0x1f, 0x22),
            raised: c(0x36, 0x33, 0x37),
            hover: c(0x40, 0x3e, 0x41),
            selected: c(0x5b, 0x59, 0x5c),
            cursor: c(0x69, 0x67, 0x6c),
            accent: c(0xff, 0xd8, 0x66),
            text: c(0xfc, 0xfc, 0xfa),
            text_dim: c(0x93, 0x92, 0x93),
            border: c(0x40, 0x3e, 0x41),
            danger: c(0xff, 0x61, 0x88),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Everblush => Palette {
            dark: true,
            bg: c(0x10, 0x16, 0x19),
            panel: c(0x14, 0x1b, 0x1e),
            sidebar: c(0x10, 0x16, 0x19),
            raised: c(0x1b, 0x22, 0x25),
            hover: c(0x23, 0x2a, 0x2d),
            selected: c(0x2d, 0x34, 0x37),
            cursor: c(0x3b, 0x42, 0x45),
            accent: c(0x67, 0xb0, 0xe8),
            text: c(0xda, 0xda, 0xda),
            text_dim: c(0x89, 0x93, 0x96),
            border: c(0x23, 0x2a, 0x2d),
            danger: c(0xe5, 0x74, 0x74),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Jellybeans => Palette {
            dark: true,
            bg: c(0x0e, 0x0e, 0x0e),
            panel: c(0x15, 0x15, 0x15),
            sidebar: c(0x0e, 0x0e, 0x0e),
            raised: c(0x1c, 0x1c, 0x1c),
            hover: c(0x26, 0x26, 0x26),
            selected: c(0x33, 0x33, 0x33),
            cursor: c(0x40, 0x40, 0x40),
            accent: c(0x81, 0x97, 0xbf),
            text: c(0xe8, 0xe8, 0xd3),
            text_dim: c(0x88, 0x88, 0x88),
            border: c(0x26, 0x26, 0x26),
            danger: c(0xcf, 0x6a, 0x4c),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Srcery => Palette {
            dark: true,
            bg: c(0x14, 0x13, 0x12),
            panel: c(0x1c, 0x1b, 0x19),
            sidebar: c(0x14, 0x13, 0x12),
            raised: c(0x26, 0x26, 0x26),
            hover: c(0x30, 0x30, 0x30),
            selected: c(0x3a, 0x3a, 0x3a),
            cursor: c(0x4e, 0x4e, 0x4e),
            accent: c(0xfb, 0xb8, 0x29),
            text: c(0xfc, 0xe8, 0xc3),
            text_dim: c(0x91, 0x81, 0x75),
            border: c(0x30, 0x30, 0x30),
            danger: c(0xef, 0x2f, 0x27),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Terafox => Palette {
            dark: true,
            bg: c(0x0f, 0x1c, 0x1e),
            panel: c(0x15, 0x25, 0x28),
            sidebar: c(0x0f, 0x1c, 0x1e),
            raised: c(0x1d, 0x33, 0x37),
            hover: c(0x25, 0x41, 0x47),
            selected: c(0x29, 0x3e, 0x40),
            cursor: c(0x42, 0x5e, 0x5e),
            accent: c(0x5a, 0x93, 0xaa),
            text: c(0xe6, 0xea, 0xea),
            text_dim: c(0x7f, 0x91, 0x95),
            border: c(0x1d, 0x33, 0x37),
            danger: c(0xe8, 0x5c, 0x51),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::EdgeDark => Palette {
            dark: true,
            bg: c(0x22, 0x23, 0x27),
            panel: c(0x2c, 0x2e, 0x34),
            sidebar: c(0x22, 0x23, 0x27),
            raised: c(0x33, 0x35, 0x3f),
            hover: c(0x36, 0x39, 0x44),
            selected: c(0x3b, 0x3e, 0x48),
            cursor: c(0x41, 0x45, 0x50),
            accent: c(0x6c, 0xb6, 0xeb),
            text: c(0xc5, 0xcd, 0xd9),
            text_dim: c(0x7f, 0x84, 0x90),
            border: c(0x36, 0x39, 0x44),
            danger: c(0xec, 0x72, 0x79),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Eighties => Palette {
            dark: true,
            bg: c(0x25, 0x25, 0x25),
            panel: c(0x2d, 0x2d, 0x2d),
            sidebar: c(0x25, 0x25, 0x25),
            raised: c(0x39, 0x39, 0x39),
            hover: c(0x39, 0x39, 0x39),
            selected: c(0x51, 0x51, 0x51),
            cursor: c(0x5f, 0x5f, 0x5f),
            accent: c(0x66, 0x99, 0xcc),
            text: c(0xcc, 0xcc, 0xcc),
            text_dim: c(0x99, 0x99, 0x99),
            border: c(0x39, 0x39, 0x39),
            danger: c(0xf2, 0x77, 0x7a),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Panda => Palette {
            dark: true,
            bg: c(0x1f, 0x20, 0x21),
            panel: c(0x29, 0x2a, 0x2b),
            sidebar: c(0x1f, 0x20, 0x21),
            raised: c(0x31, 0x35, 0x3a),
            hover: c(0x31, 0x35, 0x3a),
            selected: c(0x3e, 0x42, 0x49),
            cursor: c(0x4f, 0x53, 0x59),
            accent: c(0xff, 0x75, 0xb5),
            text: c(0xe6, 0xe6, 0xe6),
            text_dim: c(0x8a, 0x8f, 0x9b),
            border: c(0x31, 0x35, 0x3a),
            danger: c(0xff, 0x2c, 0x6d),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Cyberdream => Palette {
            dark: true,
            bg: c(0x10, 0x12, 0x14),
            panel: c(0x16, 0x18, 0x1a),
            sidebar: c(0x10, 0x12, 0x14),
            raised: c(0x1e, 0x21, 0x24),
            hover: c(0x26, 0x29, 0x2d),
            selected: c(0x3c, 0x40, 0x48),
            cursor: c(0x4b, 0x50, 0x5a),
            accent: c(0x5e, 0xa1, 0xff),
            text: c(0xff, 0xff, 0xff),
            text_dim: c(0x7b, 0x84, 0x96),
            border: c(0x1e, 0x21, 0x24),
            danger: c(0xff, 0x6e, 0x5e),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Mariana => Palette {
            dark: true,
            bg: c(0x27, 0x2e, 0x36),
            panel: c(0x30, 0x38, 0x41),
            sidebar: c(0x27, 0x2e, 0x36),
            raised: c(0x34, 0x3d, 0x46),
            hover: c(0x3b, 0x45, 0x50),
            selected: c(0x4e, 0x5a, 0x65),
            cursor: c(0x5c, 0x69, 0x75),
            accent: c(0x66, 0x99, 0xcc),
            text: c(0xd8, 0xde, 0xe9),
            text_dim: c(0xa6, 0xac, 0xb9),
            border: c(0x3b, 0x45, 0x50),
            danger: c(0xec, 0x5f, 0x66),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::Embark => Palette {
            dark: true,
            bg: c(0x17, 0x15, 0x2a),
            panel: c(0x1e, 0x1c, 0x31),
            sidebar: c(0x17, 0x15, 0x2a),
            raised: c(0x2d, 0x2b, 0x40),
            hover: c(0x2d, 0x2b, 0x40),
            selected: c(0x3e, 0x38, 0x59),
            cursor: c(0x58, 0x52, 0x73),
            accent: c(0xa3, 0x7a, 0xcc),
            text: c(0xcb, 0xe3, 0xe7),
            text_dim: c(0x8a, 0x88, 0x9d),
            border: c(0x2d, 0x2b, 0x40),
            danger: c(0xf4, 0x8f, 0xb1),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::SpacemacsDark => Palette {
            dark: true,
            bg: c(0x21, 0x20, 0x26),
            panel: c(0x29, 0x2b, 0x2e),
            sidebar: c(0x21, 0x20, 0x26),
            raised: c(0x34, 0x32, 0x3e),
            hover: c(0x34, 0x32, 0x3e),
            selected: c(0x44, 0x41, 0x55),
            cursor: c(0x54, 0x4a, 0x65),
            accent: c(0xbc, 0x6e, 0xc5),
            text: c(0xbc, 0xbc, 0xbc),
            text_dim: c(0x88, 0x88, 0x88),
            border: c(0x34, 0x32, 0x3e),
            danger: c(0xf2, 0x24, 0x1f),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::MelangeDark => Palette {
            dark: true,
            bg: c(0x22, 0x1f, 0x1d),
            panel: c(0x29, 0x25, 0x22),
            sidebar: c(0x22, 0x1f, 0x1d),
            raised: c(0x34, 0x30, 0x2c),
            hover: c(0x34, 0x30, 0x2c),
            selected: c(0x40, 0x3a, 0x36),
            cursor: c(0x4d, 0x45, 0x40),
            accent: c(0xe4, 0x9b, 0x5d),
            text: c(0xec, 0xe1, 0xd7),
            text_dim: c(0xa3, 0x8d, 0x78),
            border: c(0x34, 0x30, 0x2c),
            danger: c(0xd4, 0x77, 0x66),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(true),
        },
        ThemeName::EdgeLight => Palette {
            dark: false,
            bg: c(0xee, 0xf1, 0xf4),
            panel: c(0xfa, 0xfa, 0xfa),
            sidebar: c(0xee, 0xf1, 0xf4),
            raised: c(0xff, 0xff, 0xff),
            hover: c(0xe8, 0xeb, 0xf0),
            selected: c(0xdd, 0xe2, 0xe7),
            cursor: c(0xbc, 0xc5, 0xcf),
            accent: c(0x50, 0x79, 0xbe),
            text: c(0x4b, 0x50, 0x5b),
            text_dim: c(0x7f, 0x88, 0x98),
            border: c(0xdd, 0xe2, 0xe7),
            danger: c(0xd0, 0x58, 0x58),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::MonokaiProLight => Palette {
            dark: false,
            bg: c(0xed, 0xe7, 0xe5),
            panel: c(0xfa, 0xf4, 0xf2),
            sidebar: c(0xed, 0xe7, 0xe5),
            raised: c(0xff, 0xff, 0xff),
            hover: c(0xed, 0xe7, 0xe5),
            selected: c(0xe0, 0xd9, 0xd7),
            cursor: c(0xd3, 0xcd, 0xcc),
            accent: c(0xe1, 0x47, 0x75),
            text: c(0x29, 0x24, 0x2a),
            text_dim: c(0x8a, 0x85, 0x87),
            border: c(0xe0, 0xd9, 0xd7),
            danger: c(0xe1, 0x47, 0x75),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::Alucard => Palette {
            dark: false,
            bg: c(0xf3, 0xee, 0xda),
            panel: c(0xff, 0xfb, 0xeb),
            sidebar: c(0xf3, 0xee, 0xda),
            raised: c(0xff, 0xff, 0xff),
            hover: c(0xef, 0xe9, 0xd5),
            selected: c(0xcf, 0xcf, 0xde),
            cursor: c(0xb8, 0xb8, 0xcf),
            accent: c(0x64, 0x4a, 0xc9),
            text: c(0x1f, 0x1f, 0x1f),
            text_dim: c(0x63, 0x5d, 0x97),
            border: c(0xe6, 0xe0, 0xcb),
            danger: c(0xcb, 0x3a, 0x2a),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::Zenbones => Palette {
            dark: false,
            bg: c(0xe6, 0xe2, 0xe0),
            panel: c(0xf0, 0xed, 0xec),
            sidebar: c(0xe6, 0xe2, 0xe0),
            raised: c(0xf7, 0xf5, 0xf4),
            hover: c(0xe4, 0xdf, 0xdd),
            selected: c(0xcb, 0xd9, 0xe3),
            cursor: c(0xb6, 0xc9, 0xd6),
            accent: c(0x28, 0x64, 0x86),
            text: c(0x2c, 0x36, 0x3c),
            text_dim: c(0x7c, 0x6f, 0x69),
            border: c(0xd9, 0xd3, 0xcf),
            danger: c(0xa8, 0x33, 0x4c),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::SpacemacsLight => Palette {
            dark: false,
            bg: c(0xef, 0xea, 0xe9),
            panel: c(0xfb, 0xf8, 0xef),
            sidebar: c(0xef, 0xea, 0xe9),
            raised: c(0xff, 0xff, 0xff),
            hover: c(0xef, 0xea, 0xe9),
            selected: c(0xd3, 0xd3, 0xe7),
            cursor: c(0xc3, 0xc3, 0xdb),
            accent: c(0x6c, 0x31, 0x63),
            text: c(0x57, 0x48, 0x62),
            text_dim: c(0x8b, 0x7e, 0x8e),
            border: c(0xe3, 0xde, 0xdd),
            danger: c(0xf2, 0x24, 0x1f),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::Leuven => Palette {
            dark: false,
            bg: c(0xf3, 0xf3, 0xf3),
            panel: c(0xff, 0xff, 0xff),
            sidebar: c(0xf3, 0xf3, 0xf3),
            raised: c(0xff, 0xff, 0xff),
            hover: c(0xec, 0xec, 0xec),
            selected: c(0xd6, 0xe4, 0xfc),
            cursor: c(0xb4, 0xcc, 0xf5),
            accent: c(0x33, 0x5e, 0xa8),
            text: c(0x33, 0x33, 0x33),
            text_dim: c(0x8d, 0x8d, 0x84),
            border: c(0xe1, 0xe1, 0xe1),
            danger: c(0xc3, 0x08, 0x0d),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
        ThemeName::DoomOneLight => Palette {
            dark: false,
            bg: c(0xf0, 0xf0, 0xf0),
            panel: c(0xfa, 0xfa, 0xfa),
            sidebar: c(0xf0, 0xf0, 0xf0),
            raised: c(0xff, 0xff, 0xff),
            hover: c(0xe7, 0xe7, 0xe7),
            selected: c(0xdf, 0xdf, 0xdf),
            cursor: c(0xc6, 0xc7, 0xc7),
            accent: c(0x40, 0x78, 0xf2),
            text: c(0x38, 0x3a, 0x42),
            text_dim: c(0x7c, 0x80, 0x85),
            border: c(0xdf, 0xdf, 0xdf),
            danger: c(0xe4, 0x56, 0x49),
            chrome: z,
            alt: z,
            row_hover: z,
            syntax: Syntax::classic(false),
        },
    }
}

pub fn apply(ctx: &egui::Context, p: &Palette, font_size: f32, spacing: f32, animations: bool) {
    let mut v = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.panel;
    v.window_fill = p.raised;
    v.extreme_bg_color = p.bg;
    v.faint_bg_color = p.raised;
    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.selected;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = CornerRadius::same(6);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    // Slider rails, checkboxes and text fields sit on raised popups; keep them darker.
    v.widgets.inactive.bg_fill = p.bg;
    v.slider_trailing_fill = true;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.hovered.weak_bg_fill = p.hover;
    v.widgets.hovered.bg_fill = p.hover;
    v.widgets.hovered.bg_stroke = Stroke::NONE;
    v.widgets.active.weak_bg_fill = p.selected;
    v.widgets.active.bg_fill = p.selected;
    v.widgets.open.weak_bg_fill = p.hover;
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        use egui::{FontId, TextStyle};
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(font_size - 2.5)),
            (TextStyle::Body, FontId::proportional(font_size)),
            (TextStyle::Button, FontId::proportional(font_size)),
            (TextStyle::Heading, FontId::proportional(font_size + 5.0)),
            (TextStyle::Monospace, FontId::monospace(font_size - 1.0)),
        ]
        .into();
        // Whole points: a fractional gap (9.6 at Spacing 1.6) puts every widget after it
        // between pixels, softening edges.
        s.spacing.item_spacing = vec2((6.0 * spacing).round(), (4.0 * spacing).round());
        s.spacing.button_padding = vec2((8.0 * spacing).round(), (3.0 * spacing).round());
        s.spacing.interact_size.y = (20.0 * spacing).round();
        s.animation_time = if animations { 0.12 } else { 0.0 };
        s.interaction.selectable_labels = false;
    });
}

/// Codepoints every Nerd Font has: the powerline branch, a house and a folder.
const NERD_CHARS: &str = ":charset=e0a0 f015 f07b";

/// From `fc-list` lines of style|family|file, a Nerd Font's regular face: the symbols-only
/// one first, then the full-size ones. A Mono face shrinks each icon into one cell, half
/// the size a prompt means it at; the terminal keeps text on its columns either way.
pub fn pick_nerd_font(list: &str) -> Option<&str> {
    let rank = |family: &str| match family {
        "Symbols Nerd Font" => 0,
        f if f.ends_with(" Nerd Font") => 1,
        "Symbols Nerd Font Mono" => 2,
        f if f.ends_with("Nerd Font Mono") => 3,
        _ => 4,
    };
    list.lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '|');
            Some((it.next()?, it.next()?, it.next()?))
        })
        .filter(|(style, ..)| *style == "Regular")
        .min_by_key(|&(_, family, file)| (rank(family), family, file))
        .map(|(.., file)| file)
}

/// Fonts come from the system (so text matches the desktop); a tiny embedded
/// Noto subset (OFL) is the last resort so the UI always renders.
pub fn install_fonts(ctx: &egui::Context, family: &str, mono_family: &str) {
    const SANS: &[u8] = include_bytes!("../assets/fallback-sans.ttf");
    const MONO: &[u8] = include_bytes!("../assets/fallback-mono.ttf");
    const SYMBOLS: &[u8] = include_bytes!("../assets/fallback-symbols.ttf");
    // System fonts are memory-mapped, not read: egui then uses the bytes in place (no
    // copy) and the kernel only pages in the glyph tables actually used, sharing them
    // with every other app (the CJK fallback alone is ~20 MB).
    let first = |paths: &[&str]| paths.iter().find_map(|p| map_font(std::path::Path::new(p)));
    let fc_match = |family: &str| -> Option<&'static [u8]> {
        let out = std::process::Command::new("fc-match").args(["--format=%{file}", family]).output().ok()?;
        map_font(std::path::Path::new(String::from_utf8_lossy(&out.stdout).trim()))
    };
    let mut fonts = FontDefinitions::empty();
    let mut prop: Vec<String> = vec![];
    let mut mono: Vec<String> = vec![];
    let mut add = |fonts: &mut FontDefinitions, name: &str, bytes: &'static [u8], list: &mut Vec<String>| {
        fonts.font_data.insert(name.into(), FontData::from_static(bytes).into());
        list.push(name.into());
    };
    let custom = if family.trim().is_empty() { None } else { fc_match(family.trim()) };
    // Follow the desktop's UI font: the GTK font setting, then
    // fontconfig's system-ui alias, then a list of common sans families.
    let desktop = desktop_font_family().and_then(|f| fc_match(&f)).or_else(|| fc_match("system-ui"));
    let ui = custom.or(desktop).or_else(|| first(&[
        "/usr/share/fonts/inter/InterVariable.ttf",
        "/usr/share/fonts/inter/Inter-Regular.ttf",
        "/usr/share/fonts/TTF/Inter-Regular.ttf",
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/google-noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/cantarell/Cantarell-VF.otf",
    ]))
    .or_else(|| fc_match("sans-serif"));
    if let Some(b) = ui {
        add(&mut fonts, "ui", b, &mut prop);
    }
    // Symbols (arrows, stars, blocks) from the system when available.
    if let Some(b) = first(&["/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf", "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf", "/usr/share/fonts/TTF/DejaVuSans.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"]) {
        add(&mut fonts, "symbols", b, &mut prop);
    }
    let custom_mono = if mono_family.trim().is_empty() { None } else { fc_match(mono_family.trim()) };
    let sys_mono = custom_mono.or_else(|| first(&[
        "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
        "/usr/share/fonts/TTF/Hack-Regular.ttf",
        "/usr/share/fonts/liberation/LiberationMono-Regular.ttf",
    ]))
    .or_else(|| fc_match("monospace"));
    if let Some(b) = sys_mono {
        add(&mut fonts, "mono", b, &mut mono);
        prop.push("mono".into()); // box-drawing and arrows for proportional text too
    }
    // Shell prompts and system summaries draw icons from the Nerd Fonts' private-use
    // range, which no ordinary font fills: a Nerd Font on the system does, in the terminal.
    let nerd = std::process::Command::new("fc-list").args(["--format=%{style[0]}|%{family[0]}|%{file}\n", NERD_CHARS]).output().ok();
    if let Some(b) = nerd.and_then(|o| pick_nerd_font(&String::from_utf8_lossy(&o.stdout)).and_then(|f| map_font(std::path::Path::new(f)))) {
        add(&mut fonts, "nerd", b, &mut mono);
    }
    for p in ["/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"] {
        if let Some(b) = map_font(std::path::Path::new(p)) {
            add(&mut fonts, "cjk", b, &mut prop);
            break;
        }
    }
    let fc_match_exact = |pattern: &str, expected: &str| -> Option<&'static [u8]> {
        let out = std::process::Command::new("fc-match").args(["--format=%{file}|%{family}", pattern]).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut parts = text.trim().splitn(2, '|');
        let file = parts.next()?;
        let family = parts.next().unwrap_or_default();
        if family.to_lowercase().contains(&expected.to_lowercase()) {
            map_font(std::path::Path::new(file))
        } else {
            None
        }
    };
    let fc_match_lang = |lang: &str| -> Option<&'static [u8]> {
        let pattern = format!(":lang={lang}");
        let out = std::process::Command::new("fc-match").args(["--format=%{file}|%{lang}", &pattern]).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut parts = text.trim().splitn(2, '|');
        let file = parts.next()?;
        let langs = parts.next().unwrap_or_default();
        if langs.split('|').any(|l| l.split('-').next().unwrap_or("") == lang) {
            map_font(std::path::Path::new(file))
        } else {
            None
        }
    };
    let arabic_font = fc_match_exact("Noto Sans Arabic UI", "Noto Sans Arabic UI")
        .or_else(|| fc_match_exact("Noto Sans Arabic", "Noto Sans Arabic"))
        .or_else(|| fc_match_lang("fa"))
        .or_else(|| fc_match_lang("ar"))
        .or_else(|| first(&[
            "/usr/share/fonts/noto/NotoSansArabicUI-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansArabic-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansArabicUI-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansArabic-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
        ]));
    if let Some(b) = arabic_font {
        add(&mut fonts, "arabic", b, &mut prop);
    }
    // Interface glyphs: a Lucide subset (ISC), drawn as text in its own family.
    const ICONS: &[u8] = include_bytes!("../assets/lucide-subset.ttf");
    fonts.font_data.insert("lucide".into(), FontData::from_static(ICONS).into());
    fonts.families.insert(FontFamily::Name("icons".into()), vec!["lucide".into()]);
    for (name, bytes) in [("fb-sans", SANS), ("fb-mono", MONO), ("fb-symbols", SYMBOLS)] {
        fonts.font_data.insert(name.into(), FontData::from_static(bytes).into());
    }
    prop.extend(["fb-sans", "fb-symbols", "fb-mono"].map(String::from));
    mono.push("fb-mono".into());
    mono.extend(prop.iter().filter(|n| *n != "mono").cloned());
    fonts.families.insert(FontFamily::Proportional, prop);
    fonts.families.insert(FontFamily::Monospace, mono);
    ctx.set_fonts(fonts);
}

// ---------------------------------------------------------------- icons

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FileCat {
    Folder,
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Doc,
    Text,
    Sheet,
    Slides,
    Pdf,
    Font,
    Disk,
    Book,
    Exec,
    Other,
}

pub fn categorize(ext: &str, is_dir: bool, exec: bool) -> FileCat {
    if is_dir {
        return FileCat::Folder;
    }
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "ico" | "tif" | "tiff" | "heic" | "avif" | "psd" | "xcf" => FileCat::Image,
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "wmv" | "flv" | "m4v" => FileCat::Video,
        "mp3" | "flac" | "wav" | "ogg" | "opus" | "m4a" | "aac" | "wma" => FileCat::Audio,
        "zip" | "tar" | "gz" | "xz" | "zst" | "bz2" | "7z" | "rar" | "tgz" | "txz" | "lz" | "lzma" | "cab" | "deb" | "rpm" | "pkg" | "apk" | "jar" | "whl" | "crate" => FileCat::Archive,
        "iso" | "img" | "dmg" | "qcow2" | "vmdk" | "vdi" | "vhd" | "vhdx" | "appimage" | "flatpak" | "snap" => FileCat::Disk,
        "rs" | "c" | "h" | "cpp" | "cc" | "hpp" | "py" | "js" | "mjs" | "ts" | "tsx" | "jsx" | "go" | "java" | "kt" | "rb" | "php" | "sh" | "bash" | "fish" | "zsh" | "lua" | "json" | "toml" | "yaml" | "yml" | "xml" | "html" | "htm" | "css" | "scss" | "sql" | "zig" | "cs" | "swift" | "conf" | "ini" | "cfg" | "vue" | "svelte" | "dart" | "hs" | "ex" | "nix" | "cmake" | "mk" | "gradle" | "diff" | "patch" => FileCat::Code,
        "txt" | "md" | "markdown" | "log" | "nfo" | "rst" | "org" | "srt" | "vtt" => FileCat::Text,
        "doc" | "docx" | "odt" | "rtf" | "tex" | "pages" => FileCat::Doc,
        "xls" | "xlsx" | "ods" | "csv" | "tsv" | "numbers" => FileCat::Sheet,
        "ppt" | "pptx" | "odp" | "key" => FileCat::Slides,
        "epub" | "mobi" | "azw3" | "djvu" | "fb2" | "cbz" | "cbr" => FileCat::Book,
        "ttf" | "otf" | "woff" | "woff2" | "ttc" | "pfb" => FileCat::Font,
        "pdf" => FileCat::Pdf,
        _ if exec => FileCat::Exec,
        _ => FileCat::Other,
    }
}

pub fn cat_color(c: FileCat) -> Color32 {
    let c32 = Color32::from_rgb;
    match c {
        FileCat::Folder => c32(0xf2, 0xc0, 0x4f),
        FileCat::Image => c32(0x4f, 0xc3, 0x8a),
        FileCat::Video => c32(0xe0, 0x5f, 0x8a),
        FileCat::Audio => c32(0xb0, 0x7c, 0xf0),
        FileCat::Archive => c32(0xd9, 0x8a, 0x4a),
        FileCat::Code => c32(0x58, 0xb4, 0xe8),
        FileCat::Doc => c32(0x4a, 0x86, 0xe8),
        FileCat::Text => c32(0xa8, 0xb2, 0xc2),
        FileCat::Sheet => c32(0x3f, 0xb3, 0x6b),
        FileCat::Slides => c32(0xf0, 0x8a, 0x3c),
        FileCat::Font => c32(0xd0, 0x6d, 0xc8),
        FileCat::Disk => c32(0x7f, 0x8f, 0xa8),
        FileCat::Book => c32(0xc9, 0x7b, 0x52),
        FileCat::Pdf => c32(0xe8, 0x5a, 0x4f),
        FileCat::Exec => c32(0x7c, 0xd6, 0x5a),
        FileCat::Other => c32(0x8a, 0x93, 0xa3),
    }
}

/// Draw a folder or document glyph centered in `rect`.
pub fn paint_icon(painter: &Painter, rect: Rect, cat: FileCat, ext: &str, is_link: bool) {
    let s = rect.width().min(rect.height());
    let c = rect.center();
    let col = cat_color(cat);
    if cat == FileCat::Folder {
        let w = s * 0.92;
        let h = s * 0.72;
        let r = Rect::from_center_size(c + vec2(0.0, s * 0.04), vec2(w, h));
        let tab = Rect::from_min_size(r.min - vec2(0.0, s * 0.08), vec2(w * 0.42, s * 0.2));
        let rad = CornerRadius::same((s * 0.08).max(1.0) as u8);
        painter.rect_filled(tab, rad, col.gamma_multiply(0.75));
        painter.rect_filled(r, rad, col);
        let front = Rect::from_min_max(pos2(r.min.x, r.min.y + h * 0.22), r.max);
        painter.rect_filled(front, rad, lighten(col, 0.12));
    } else {
        let w = s * 0.72;
        let h = s * 0.92;
        let r = Rect::from_center_size(c, vec2(w, h));
        let fold = s * 0.24;
        let pts = vec![
            r.left_top(),
            pos2(r.right() - fold, r.top()),
            pos2(r.right(), r.top() + fold),
            r.right_bottom(),
            r.left_bottom(),
        ];
        painter.add(Shape::convex_polygon(pts, col.gamma_multiply(0.9), Stroke::NONE));
        painter.add(Shape::convex_polygon(
            vec![pos2(r.right() - fold, r.top()), pos2(r.right(), r.top() + fold), pos2(r.right() - fold, r.top() + fold)],
            lighten(col, 0.35),
            Stroke::NONE,
        ));
        // A symbol for the kind of file, drawn on the page.
        if s >= 20.0 {
            let k = s * 0.22;
            let m = pos2(c.x, r.top() + h * (if s >= 28.0 && !ext.is_empty() { 0.42 } else { 0.55 }));
            let ink = Color32::from_rgba_unmultiplied(255, 255, 255, 215);
            let st = Stroke::new((s / 22.0).max(1.0), ink);
            let pt = |x: f32, y: f32| m + vec2(x * k, y * k);
            match cat {
                FileCat::Image => {
                    painter.add(Shape::convex_polygon(vec![pt(-1.1, 0.8), pt(-0.2, -0.4), pt(0.4, 0.3), pt(0.7, 0.0), pt(1.1, 0.8)], ink, Stroke::NONE));
                    painter.circle_filled(pt(0.6, -0.6), k * 0.25, ink);
                }
                FileCat::Video => {
                    painter.add(Shape::convex_polygon(vec![pt(-0.6, -0.8), pt(0.9, 0.0), pt(-0.6, 0.8)], ink, Stroke::NONE));
                }
                FileCat::Audio => {
                    painter.line_segment([pt(-0.3, 0.6), pt(-0.3, -0.8)], st);
                    painter.line_segment([pt(-0.3, -0.8), pt(0.7, -1.0)], st);
                    painter.line_segment([pt(0.7, -1.0), pt(0.7, 0.4)], st);
                    painter.circle_filled(pt(-0.55, 0.65), k * 0.32, ink);
                    painter.circle_filled(pt(0.45, 0.45), k * 0.32, ink);
                }
                FileCat::Archive => {
                    for i in 0..4 {
                        let y = -1.0 + i as f32 * 0.45;
                        painter.line_segment([pt(-0.25, y), pt(0.25, y)], st);
                    }
                    painter.rect_stroke(Rect::from_min_max(pt(-0.35, 0.8), pt(0.35, 1.3)), CornerRadius::same(1), st, StrokeKind::Middle);
                }
                FileCat::Code => {
                    painter.add(Shape::line(vec![pt(-0.4, -0.6), pt(-1.0, 0.0), pt(-0.4, 0.6)], st));
                    painter.add(Shape::line(vec![pt(0.4, -0.6), pt(1.0, 0.0), pt(0.4, 0.6)], st));
                    painter.line_segment([pt(0.2, -0.8), pt(-0.2, 0.8)], st);
                }
                FileCat::Text | FileCat::Doc => {
                    for (i, w) in [1.0f32, 0.7, 1.0, 0.55].iter().enumerate() {
                        let y = -0.75 + i as f32 * 0.5;
                        painter.line_segment([pt(-0.9, y), pt(-0.9 + 1.8 * w, y)], st);
                    }
                }
                FileCat::Sheet => {
                    painter.rect_stroke(Rect::from_min_max(pt(-0.9, -0.8), pt(0.9, 0.8)), CornerRadius::same(1), st, StrokeKind::Middle);
                    painter.line_segment([pt(-0.9, 0.0), pt(0.9, 0.0)], st);
                    painter.line_segment([pt(-0.2, -0.8), pt(-0.2, 0.8)], st);
                }
                FileCat::Slides => {
                    for (i, hgt) in [0.6f32, 1.2, 0.9].iter().enumerate() {
                        let x = -0.75 + i as f32 * 0.6;
                        painter.rect_filled(Rect::from_min_max(pt(x, 0.8 - hgt), pt(x + 0.35, 0.8)), CornerRadius::ZERO, ink);
                    }
                }
                FileCat::Pdf => {
                    painter.add(Shape::closed_line(vec![pt(0.0, -1.0), pt(0.9, 0.7), pt(-0.9, 0.7)], st));
                }
                FileCat::Font => {
                    painter.text(m, egui::Align2::CENTER_CENTER, "Aa", egui::FontId::proportional(k * 1.9), ink);
                }
                FileCat::Disk => {
                    painter.circle_stroke(m, k * 1.0, st);
                    painter.circle_filled(m, k * 0.25, ink);
                }
                FileCat::Book => {
                    painter.add(Shape::closed_line(vec![pt(-1.0, -0.7), pt(0.0, -0.45), pt(1.0, -0.7), pt(1.0, 0.8), pt(0.0, 1.0), pt(-1.0, 0.8)], st));
                    painter.line_segment([pt(0.0, -0.45), pt(0.0, 1.0)], st);
                }
                FileCat::Exec => {
                    painter.add(Shape::line(vec![pt(-0.9, -0.6), pt(-0.2, 0.0), pt(-0.9, 0.6)], st));
                    painter.line_segment([pt(0.0, 0.7), pt(0.9, 0.7)], st);
                }
                _ => {}
            }
        }
        if s >= 28.0 && !ext.is_empty() {
            let label: String = ext.chars().take(4).collect::<String>().to_uppercase();
            painter.text(
                pos2(c.x, r.bottom() - s * 0.17),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional((s * 0.18).clamp(7.0, 20.0)),
                Color32::from_rgba_unmultiplied(0, 0, 0, 160),
            );
        }
    }
    if is_link {
        let a = Rect::from_min_size(pos2(rect.left(), rect.bottom() - s * 0.38), Vec2::splat(s * 0.38));
        painter.rect(a, CornerRadius::same(2), Color32::WHITE, Stroke::new(1.0, Color32::DARK_GRAY), StrokeKind::Inside);
        painter.line_segment([a.left_bottom() + vec2(s * 0.08, -s * 0.08), a.right_top() + vec2(-s * 0.08, s * 0.08)], Stroke::new(1.5, Color32::BLACK));
    }
}

fn lighten(c: Color32, t: f32) -> Color32 {
    let f = |v: u8| (v as f32 + (255.0 - v as f32) * t) as u8;
    Color32::from_rgb(f(c.r()), f(c.g()), f(c.b()))
}

/// Small line-art glyphs for the sidebar & toolbar.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    Home,
    Desktop,
    Docs,
    Down,
    Pics,
    Music,
    Video,
    Drive,
    Trash,
    Pin,
    Root,
    Back,
    Forward,
    Up,
    Refresh,
    Split,
    Search,
    Eye,
    Plus,
    Close,
    Clock,
    Bookmark,
    Folder,
    Monitor,
    CollapseLeft,
    Menu,
    ChevronRight,
    ChevronDown,
    Lock,
    LockOpen,
    Usb,
    Eject,
    Globe,
    Phone,
    Wrap,
    Tag,
    Cloud,
    ChevronUp,
    List,
    ListTree,
    Maximize,
    Code,
    Table,
    Blame,
    Follow,
    Copy,
}

/// Draw a Lucide icon by name, centered in `rect` (the icon's stroke box roughly fills it).
pub fn paint_lucide(painter: &Painter, rect: Rect, name: &str, col: Color32) -> bool {
    let Some(&(_, ch)) = crate::lucide_map::LUCIDE.iter().find(|(n, _)| *n == name) else { return false };
    let s = rect.width().min(rect.height());
    // Lucide draws inside a 24-unit box with 2 units of padding: scale so the strokes span `s`.
    let size = (s * 24.0 / 20.0).round().max(6.0);
    let g = painter.layout_no_wrap(ch.to_string(), egui::FontId::new(size, FontFamily::Name("icons".into())), col);
    let pos = rect.center() - g.rect.size() / 2.0;
    painter.galley(pos2(pos.x.round(), pos.y.round()), g, col);
    true
}

/// Lucide name for each interface glyph.
fn glyph_name(g: Glyph) -> &'static str {
    match g {
        Glyph::Home => "house",
        Glyph::Desktop => "monitor",
        Glyph::Docs => "file-text",
        Glyph::Down => "download",
        Glyph::Pics => "image",
        Glyph::Music => "music",
        Glyph::Video => "video",
        Glyph::Drive => "hard-drive",
        Glyph::Trash => "trash-2",
        Glyph::Pin => "pin",
        Glyph::Root => "slash",
        Glyph::Back => "arrow-left",
        Glyph::Forward => "arrow-right",
        Glyph::ChevronRight => "chevron-right",
        Glyph::ChevronDown => "chevron-down",
        Glyph::Lock => "lock",
        Glyph::LockOpen => "lock-open",
        Glyph::Up => "arrow-up",
        Glyph::Refresh => "rotate-ccw",
        Glyph::Split => "columns-2",
        Glyph::Search => "search",
        Glyph::Eye => "scan-eye",
        Glyph::Plus => "plus",
        Glyph::Close => "x",
        Glyph::Clock => "clock",
        Glyph::Bookmark => "bookmark",
        Glyph::Folder => "folder",
        Glyph::Monitor => "monitor",
        Glyph::CollapseLeft => "panel-left-close",
        Glyph::Menu => "menu",
        Glyph::Usb => "usb",
        Glyph::Globe => "globe",
        Glyph::Phone => "smartphone",
        Glyph::Maximize => "maximize-2",
        Glyph::Table => "grid-3x3",
        Glyph::Eject | Glyph::Wrap | Glyph::Tag | Glyph::Cloud | Glyph::ChevronUp | Glyph::List | Glyph::ListTree | Glyph::Code | Glyph::Blame | Glyph::Follow | Glyph::Copy => "", // drawn below
    }
}

pub fn paint_glyph(painter: &Painter, rect: Rect, g: Glyph, col: Color32) {
    if paint_lucide(painter, rect, glyph_name(g), col) {
        return;
    }
    let s = rect.width().min(rect.height());
    let c = rect.center();
    let st = Stroke::new((s / 11.0).max(1.2), col);
    let p = |x: f32, y: f32| -> Pos2 { c + vec2(x * s, y * s) };
    let line = |a: Pos2, b: Pos2| painter.line_segment([a, b], st);
    match g {
        Glyph::Home => {
            painter.add(Shape::line(vec![p(-0.35, 0.0), p(0.0, -0.32), p(0.35, 0.0)], st));
            painter.add(Shape::line(vec![p(-0.25, -0.08), p(-0.25, 0.32), p(0.25, 0.32), p(0.25, -0.08)], st));
        }
        Glyph::Desktop => {
            painter.rect_stroke(Rect::from_min_max(p(-0.38, -0.3), p(0.38, 0.18)), CornerRadius::same(2), st, StrokeKind::Middle);
            line(p(0.0, 0.18), p(0.0, 0.32));
            line(p(-0.18, 0.32), p(0.18, 0.32));
        }
        Glyph::Docs => {
            painter.rect_stroke(Rect::from_min_max(p(-0.28, -0.36), p(0.28, 0.36)), CornerRadius::same(2), st, StrokeKind::Middle);
            for y in [-0.15, 0.0, 0.15] {
                line(p(-0.15, y), p(0.15, y));
            }
        }
        Glyph::Down => {
            line(p(0.0, -0.36), p(0.0, 0.16));
            painter.add(Shape::line(vec![p(-0.2, -0.04), p(0.0, 0.16), p(0.2, -0.04)], st));
            line(p(-0.32, 0.34), p(0.32, 0.34));
        }
        Glyph::Pics => {
            painter.rect_stroke(Rect::from_min_max(p(-0.38, -0.3), p(0.38, 0.3)), CornerRadius::same(2), st, StrokeKind::Middle);
            painter.add(Shape::line(vec![p(-0.38, 0.22), p(-0.1, -0.04), p(0.1, 0.14), p(0.2, 0.04), p(0.38, 0.2)], st));
            painter.circle_stroke(p(0.15, -0.14), s * 0.06, st);
        }
        Glyph::Music => {
            line(p(-0.12, 0.24), p(-0.12, -0.3));
            line(p(-0.12, -0.3), p(0.28, -0.36));
            line(p(0.28, -0.36), p(0.28, 0.18));
            painter.circle_filled(p(-0.2, 0.26), s * 0.1, col);
            painter.circle_filled(p(0.2, 0.2), s * 0.1, col);
        }
        Glyph::Video => {
            painter.rect_stroke(Rect::from_min_max(p(-0.38, -0.24), p(0.16, 0.24)), CornerRadius::same(2), st, StrokeKind::Middle);
            painter.add(Shape::line(vec![p(0.16, -0.06), p(0.38, -0.2), p(0.38, 0.2), p(0.16, 0.06)], st));
        }
        Glyph::Drive => {
            painter.rect_stroke(Rect::from_min_max(p(-0.38, -0.18), p(0.38, 0.22)), CornerRadius::same(3), st, StrokeKind::Middle);
            painter.circle_filled(p(0.22, 0.02), s * 0.05, col);
            line(p(-0.24, 0.02), p(0.06, 0.02));
        }
        Glyph::Trash => {
            line(p(-0.34, -0.24), p(0.34, -0.24));
            line(p(-0.1, -0.34), p(0.1, -0.34));
            painter.add(Shape::line(vec![p(-0.26, -0.24), p(-0.2, 0.36), p(0.2, 0.36), p(0.26, -0.24)], st));
        }
        Glyph::Pin => {
            painter.circle_stroke(p(0.0, -0.14), s * 0.18, st);
            line(p(0.0, 0.04), p(0.0, 0.38));
        }
        Glyph::Root => {
            line(p(-0.3, -0.3), p(0.3, 0.3));
        }
        Glyph::Back => {
            painter.add(Shape::line(vec![p(0.12, -0.3), p(-0.18, 0.0), p(0.12, 0.3)], st));
        }
        Glyph::Forward => {
            painter.add(Shape::line(vec![p(-0.12, -0.3), p(0.18, 0.0), p(-0.12, 0.3)], st));
        }
        Glyph::Up => {
            painter.add(Shape::line(vec![p(-0.3, 0.12), p(0.0, -0.18), p(0.3, 0.12)], st));
        }
        Glyph::Refresh => {
            let pts: Vec<Pos2> = (0..=20)
                .map(|i| {
                    let a = 0.6 + i as f32 / 20.0 * 5.0;
                    c + vec2(a.cos(), a.sin()) * s * 0.3
                })
                .collect();
            let end = *pts.last().unwrap();
            painter.add(Shape::line(pts, st));
            painter.add(Shape::line(vec![end + vec2(-s * 0.16, -s * 0.02), end, end + vec2(-s * 0.02, s * 0.16)], st));
        }
        Glyph::Split => {
            painter.rect_stroke(Rect::from_min_max(p(-0.38, -0.3), p(0.38, 0.3)), CornerRadius::same(2), st, StrokeKind::Middle);
            line(p(0.0, -0.3), p(0.0, 0.3));
        }
        Glyph::Search => {
            painter.circle_stroke(p(-0.06, -0.06), s * 0.24, st);
            line(p(0.12, 0.12), p(0.34, 0.34));
        }
        Glyph::Eye => {
            let top: Vec<Pos2> = (0..=16).map(|i| { let t = i as f32 / 16.0; p(-0.4 + 0.8 * t, -0.24 * (std::f32::consts::PI * t).sin()) }).collect();
            let bot: Vec<Pos2> = (0..=16).map(|i| { let t = i as f32 / 16.0; p(-0.4 + 0.8 * t, 0.24 * (std::f32::consts::PI * t).sin()) }).collect();
            painter.add(Shape::line(top, st));
            painter.add(Shape::line(bot, st));
            painter.circle_filled(c, s * 0.1, col);
        }
        Glyph::Plus => {
            line(p(-0.3, 0.0), p(0.3, 0.0));
            line(p(0.0, -0.3), p(0.0, 0.3));
        }
        Glyph::Close => {
            line(p(-0.25, -0.25), p(0.25, 0.25));
            line(p(-0.25, 0.25), p(0.25, -0.25));
        }
        Glyph::Bookmark => {
            painter.add(Shape::closed_line(vec![p(-0.24, -0.36), p(0.24, -0.36), p(0.24, 0.38), p(0.0, 0.16), p(-0.24, 0.38)], st));
        }
        Glyph::Folder => {
            painter.add(Shape::closed_line(vec![p(-0.38, -0.26), p(-0.1, -0.26), p(0.0, -0.16), p(0.38, -0.16), p(0.38, 0.3), p(-0.38, 0.3)], st));
        }
        Glyph::Monitor => {
            painter.rect_stroke(Rect::from_min_max(p(-0.4, -0.3), p(0.4, 0.18)), CornerRadius::same(2), st, StrokeKind::Middle);
            line(p(-0.16, 0.34), p(0.16, 0.34));
            line(p(0.0, 0.18), p(0.0, 0.34));
        }
        Glyph::CollapseLeft => {
            line(p(-0.34, -0.32), p(-0.34, 0.32));
            line(p(0.34, 0.0), p(-0.16, 0.0));
            painter.add(Shape::line(vec![p(0.0, -0.16), p(-0.16, 0.0), p(0.0, 0.16)], st));
        }
        Glyph::ChevronRight | Glyph::ChevronDown | Glyph::Lock | Glyph::LockOpen | Glyph::Usb | Glyph::Globe | Glyph::Phone | Glyph::Maximize | Glyph::Table => {}
        // On Lucide's 24-unit grid, so they sit with the font's icons.
        Glyph::Wrap => {
            let m = |x: f32, y: f32| p((x - 12.0) * 0.036, (y - 12.0) * 0.036);
            line(m(3.0, 6.0), m(21.0, 6.0));
            line(m(3.0, 18.0), m(10.0, 18.0));
            // Out along the middle row, round the end and back under it.
            let mut pts = vec![m(3.0, 12.0)];
            pts.extend((0..=12).map(|k| {
                let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * k as f32 / 12.0;
                m(18.0 + 3.0 * a.cos(), 15.0 + 3.0 * a.sin())
            }));
            pts.push(m(14.0, 18.0));
            painter.add(Shape::line(pts, st));
            painter.add(Shape::line(vec![m(16.0, 16.0), m(14.0, 18.0), m(16.0, 20.0)], st));
        }
        Glyph::Cloud => {
            // Lucide's cloud: a large arc round the left and top, a small one at the right.
            let m = |x: f32, y: f32| p((x - 12.0) * 0.036, (y - 12.0) * 0.036);
            let arc = |cx: f32, cy: f32, r: f32, a0: f32, a1: f32, pts: &mut Vec<Pos2>| {
                for k in 0..=14 {
                    let a = (a0 + (a1 - a0) * k as f32 / 14.0).to_radians();
                    pts.push(m(cx + r * a.cos(), cy + r * a.sin()));
                }
            };
            let mut pts = vec![m(17.5, 19.0)];
            arc(9.0, 12.0, 7.0, 90.0, 343.4, &mut pts);
            arc(17.5, 14.5, 4.5, 270.0, 450.0, &mut pts);
            painter.add(Shape::Path(egui::epaint::PathShape { points: pts, closed: true, fill: Color32::TRANSPARENT, stroke: st.into() }));
        }
        Glyph::Tag => {
            let m = |x: f32, y: f32| p((x - 12.0) * 0.036, (y - 12.0) * 0.036);
            let outline = vec![m(2.5, 2.5), m(11.2, 2.5), m(21.0, 12.3), m(12.3, 21.0), m(2.5, 11.2)];
            painter.add(Shape::Path(egui::epaint::PathShape { points: outline, closed: true, fill: Color32::TRANSPARENT, stroke: st.into() }));
            painter.circle_filled(m(7.5, 7.5), st.width * 0.9, col);
        }
        Glyph::Eject => {
            // Rounded triangle over a bar, in Lucide's stroke style.
            let tri = vec![p(0.0, -0.34), p(0.34, 0.06), p(-0.34, 0.06)];
            painter.add(Shape::Path(egui::epaint::PathShape { points: tri, closed: true, fill: Color32::TRANSPARENT, stroke: st.into() }));
            painter.rect_stroke(Rect::from_min_max(p(-0.34, 0.2), p(0.34, 0.32)), CornerRadius::same(1), st, StrokeKind::Middle);
        }
        Glyph::Menu => {
            for y in [-0.24, 0.0, 0.24] {
                line(p(-0.32, y), p(0.32, y));
            }
        }
        Glyph::ChevronUp => {
            // Lucide's chevron-up on the same scale as the font's chevron-down.
            let m = |x: f32, y: f32| p((x - 12.0) / 20.0, (y - 12.0) / 20.0);
            let st = Stroke::new(s / 10.0, col);
            painter.add(Shape::line(vec![m(6.0, 15.0), m(12.0, 9.0), m(18.0, 15.0)], st));
        }
        Glyph::Code | Glyph::Blame | Glyph::Follow | Glyph::Copy => {
            // On Lucide's 24-unit grid: angle brackets with a slash; a column of short
            // marks beside lines; an arrow down onto a line; two overlapping sheets.
            let m = |x: f32, y: f32| p((x - 12.0) / 20.0, (y - 12.0) / 20.0);
            let st = Stroke::new(s / 10.0, col);
            match g {
                Glyph::Code => {
                    painter.add(Shape::line(vec![m(18.0, 16.0), m(22.0, 12.0), m(18.0, 8.0)], st));
                    painter.add(Shape::line(vec![m(6.0, 8.0), m(2.0, 12.0), m(6.0, 16.0)], st));
                    painter.line_segment([m(14.5, 4.0), m(9.5, 20.0)], st);
                }
                Glyph::Blame => {
                    // The gutter's strip, a name where a run begins, its lines beside it.
                    painter.line_segment([m(3.0, 3.0), m(3.0, 21.0)], st);
                    painter.circle_stroke(m(8.5, 7.0), s * 0.11, st);
                    painter.line_segment([m(14.0, 7.0), m(21.0, 7.0)], st);
                    painter.line_segment([m(14.0, 13.0), m(21.0, 13.0)], st);
                    painter.line_segment([m(8.0, 19.0), m(21.0, 19.0)], st);
                }
                Glyph::Follow => {
                    painter.line_segment([m(12.0, 3.0), m(12.0, 17.0)], st);
                    painter.add(Shape::line(vec![m(6.0, 11.0), m(12.0, 17.0), m(18.0, 11.0)], st));
                    painter.line_segment([m(5.0, 21.0), m(19.0, 21.0)], st);
                }
                _ => {
                    painter.rect_stroke(Rect::from_min_max(m(8.0, 8.0), m(22.0, 22.0)), CornerRadius::same((s / 10.0).round() as u8), st, StrokeKind::Middle);
                    painter.add(Shape::line(vec![m(4.0, 16.0), m(2.0, 14.0), m(2.0, 4.0), m(4.0, 2.0), m(14.0, 2.0), m(16.0, 4.0)], st));
                }
            }
        }
        Glyph::List | Glyph::ListTree => {
            // Lucide's list (dots before three lines) and list-tree (lines branching off a stem).
            let m = |x: f32, y: f32| p((x - 12.0) / 20.0, (y - 12.0) / 20.0);
            let st = Stroke::new(s / 10.0, col);
            if g == Glyph::List {
                for y in [6.0, 12.0, 18.0] {
                    painter.circle_filled(m(3.5, y), s / 18.0, col);
                    painter.line_segment([m(8.0, y), m(21.0, y)], st);
                }
            } else {
                painter.line_segment([m(8.0, 6.0), m(21.0, 6.0)], st);
                painter.line_segment([m(13.0, 12.0), m(21.0, 12.0)], st);
                painter.line_segment([m(13.0, 18.0), m(21.0, 18.0)], st);
                painter.add(Shape::line(vec![m(4.0, 6.0), m(4.0, 17.0), m(5.0, 18.0), m(8.0, 18.0)], st));
                painter.line_segment([m(4.0, 12.0), m(8.0, 12.0)], st);
            }
        }
        Glyph::Clock => {
            painter.circle_stroke(c, s * 0.34, st);
            painter.add(Shape::line(vec![p(0.0, -0.2), p(0.0, 0.0), p(0.14, 0.1)], st));
        }
    }
}

/// Family name of the desktop UI font (GNOME/GTK setting), without the size.
fn desktop_font_family() -> Option<String> {
    let raw = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "font-name"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let cfg = dirs::config_dir()?;
            ["gtk-4.0/settings.ini", "gtk-3.0/settings.ini"].iter().find_map(|f| {
                std::fs::read_to_string(cfg.join(f)).ok()?.lines().find_map(|l| l.trim().strip_prefix("gtk-font-name=").map(|v| v.trim().trim_matches('"').to_string()))
            })
        })?;
    // "Google Sans Flex 11" -> "Google Sans Flex"
    let fam = raw.rsplit_once(' ').filter(|(_, size)| size.parse::<f32>().is_ok()).map(|(f, _)| f.to_string()).unwrap_or(raw);
    (!fam.is_empty()).then_some(fam)
}

/// Read-only shared mapping of a font file, kept for the life of the process (and reused
/// if the same file is requested again, e.g. after a font option change).
fn map_font(path: &std::path::Path) -> Option<&'static [u8]> {
    use std::os::fd::AsRawFd;
    static MAPS: std::sync::Mutex<Vec<(std::path::PathBuf, usize, usize)>> = std::sync::Mutex::new(Vec::new());
    let canon = std::fs::canonicalize(path).ok()?;
    let mut maps = MAPS.lock().ok()?;
    if let Some(&(_, ptr, len)) = maps.iter().find(|(p, ..)| *p == canon) {
        return Some(unsafe { std::slice::from_raw_parts(ptr as *const u8, len) });
    }
    let f = std::fs::File::open(&canon).ok()?;
    let len = f.metadata().ok()?.len() as usize;
    if len < 12 {
        return None;
    }
    let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, f.as_raw_fd(), 0) };
    if ptr == libc::MAP_FAILED {
        return None;
    }
    maps.push((canon, ptr as usize, len));
    Some(unsafe { std::slice::from_raw_parts(ptr as *const u8, len) })
}
