#!/usr/bin/env python3
"""把 Material Symbols Outlined 裁成一个只含本项目用到的图标的静态子集。

为什么：管理端（apps/douyin-web）与展示端（st_page.html）之前都直接引
Google Fonts 的 Material Symbols Outlined，那个可变字体约 4MB，而且 `<link
rel=stylesheet>` 是渲染阻塞 —— 国内网络一慢/一断，整页白屏。这里把两处实际
用到的图标名收集起来，裁成几十 KB 的子集自托管，去掉外部依赖与首屏阻塞。

实现要点：
  - Material Symbols 用「必需连字 rlig」把图标名（如 chevron_right）渲染成图形；
    直接 `pyftsubset --layout-features=rlig` 会因连字闭包把 4301 个连字几乎全量
    保留（子集仍 3MB+），所以这里手动重建只含本项目 45 个连字的 GSUB。
  - 两处前端只用默认轴值（FILL=0 / wght=400 / GRAD=0 / opsz=24），因此把可变
    字体实例化成静态字体（instantiateVariableFont），体积再降一个数量级。

用法（依赖 fonttools，本机已装）：
  1. 保证 ICONS 与「announceIcons.ts / st_page.html 里用到的图标名」一致，
     新加了图标名就补进来，再跑一次本脚本。
  2. `python3 scripts/fonts/subset-material-symbols.py`

产物：
  - apps/douyin-web/public/fonts/material-symbols-outlined.woff2（管理端自托管）
  - scripts/fonts/material-symbols-outlined.base64.txt（供 st_page.html 内联）
"""

import base64
import os
import subprocess
import sys
import tempfile

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

# 本项目（管理端 + 展示端）实际用到的全部 Material Symbols 图标名。
# 新加图标时同步到这里（以及两处前端的白名单/用法）。
ICONS = [
    # 管理端 announceIcons.ts 白名单
    "auto_awesome", "military_tech", "qr_code_scanner", "notifications_active",
    "insights", "tips_and_updates", "campaign", "star", "bolt", "groups",
    "emoji_events", "local_fire_department", "trending_up", "calendar_month",
    "chat_bubble", "lock", "person", "smart_toy", "workspace_premium",
    # 管理端其它硬编码图标
    "block", "check_circle", "error",
    # 展示端 st_page.html 硬编码图标
    "chevron_left", "chevron_right", "close", "history_edu", "expand_less",
    "expand_more", "search", "cancel", "filter_center_focus", "unfold_less",
    "unfold_more", "playlist_play", "smart_display", "verified",
    "monetization_on", "notifications", "flag", "lock_reset", "info", "chat",
    "arrow_back", "today", "functions",
]

_DIGITS = ("zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine")

URL = ("https://fonts.gstatic.com/s/materialsymbolsoutlined/v377/"
       "kJEhBvYX7BgnkSrUwT8OhrdQw4oELdPIeeII9v6oFsLjBuVY.woff2")


def char_to_glyph(ch: str) -> str:
    if ch == "_":
        return "underscore"
    if ch.isdigit():
        return f"digit_{_DIGITS[int(ch)]}"
    return ch


def name_to_glyph(name: str) -> str:
    """图标名 → 连字目标字形名（emoji_events → trophy 这类别名靠 GSUB 解析）。"""
    return None  # 由 build 阶段填充


def resolve(names, lig_map, glyph_set):
    resolved, missing = {}, []
    for n in names:
        if n in lig_map:
            resolved[n] = lig_map[n]
        elif n in glyph_set:
            resolved[n] = n
        else:
            missing.append(n)
    return resolved, missing


def build_ligature_map(font: TTFont) -> dict:
    gsub = font["GSUB"].table
    lookups = []
    for fr in gsub.FeatureList.FeatureRecord:
        if fr.FeatureTag == "rlig":
            lookups = fr.Feature.LookupListIndex
    subtables = []
    for li in lookups:
        lookup = gsub.LookupList.Lookup[li]
        for st in lookup.SubTable:
            ext = getattr(st, "ExtSubTable", None)
            if ext is not None:
                subtables.extend(ext if isinstance(ext, list) else [ext])
            else:
                subtables.append(st)

    def g2c(g):
        if g == "underscore":
            return "_"
        if g.startswith("digit_") and g[6:] in _DIGITS:
            return str(_DIGITS.index(g[6:]))
        return g

    out = {}
    for st in subtables:
        for first, comps in st.ligatures.items():
            for comp in comps:
                name = first + "".join(g2c(c) for c in comp.Component)
                out[name] = comp.LigGlyph
    return out


def build_minimal_gsub(font: TTFont, lig_map: dict) -> None:
    """把 GSUB 里的 rlig 精简成只含本项目图标，避免连字闭包把整表带走。"""
    from fontTools.ttLib.tables import otTables as ot

    glyph_order = font.getGlyphOrder()
    glyph_index = {name: i for i, name in enumerate(glyph_order)}

    # 按首字符分组：{ first_glyph_name: [(component_names_tuple, lig_glyph_name), ...] }
    grouped: dict[str, list[tuple[tuple[str, ...], str]]] = {}
    for name in ICONS:
        comps = tuple(char_to_glyph(c) for c in name)
        lig = lig_map[name]
        first = comps[0]
        grouped.setdefault(first, []).append((comps[1:], lig))

    lig_subst = ot.LigatureSubst()
    lig_subst.ligatures = {}
    for first, entries in grouped.items():
        ligs = []
        for comps, lig in entries:
            lg = ot.Ligature()
            lg.Component = list(comps)
            lg.LigGlyph = lig
            ligs.append(lg)
        lig_subst.ligatures[first] = ligs

    lookup = ot.Lookup()
    lookup.LookupType = 4
    lookup.LookupFlag = 0
    lookup.SubTable = [lig_subst]

    feature = ot.Feature()
    feature.FeatureParams = None
    feature.LookupListIndex = [0]

    feat_rec = ot.FeatureRecord()
    feat_rec.FeatureTag = "rlig"
    feat_rec.Feature = feature

    lang = ot.LangSys()
    lang.LookupOrder = None
    lang.ReqFeatureIndex = 0xFFFF
    lang.FeatureIndex = [0]

    script = ot.Script()
    script.DefaultLangSys = lang
    script.LangSysRecord = []

    script_rec = ot.ScriptRecord()
    script_rec.ScriptTag = "DFLT"
    script_rec.Script = script

    gsub = ot.GSUB()
    gsub.ScriptList = ot.ScriptList()
    gsub.ScriptList.ScriptRecord = [script_rec]
    gsub.ScriptList.ScriptCount = 1
    gsub.FeatureList = ot.FeatureList()
    gsub.FeatureList.FeatureRecord = [feat_rec]
    gsub.FeatureList.FeatureCount = 1
    gsub.LookupList = ot.LookupList()
    gsub.LookupList.Lookup = [lookup]
    gsub.LookupList.LookupCount = 1
    gsub.Version = 0x00010000

    font["GSUB"].table = gsub


def main() -> int:
    repo = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    with tempfile.TemporaryDirectory() as td:
        src = os.path.join(td, "full.woff2")
        print(f"下载全量字体 -> {src}")
        subprocess.run(["curl", "-s", "--max-time", "60", "-o", src, URL], check=True)

        font = TTFont(src)
        glyph_set = set(font.getGlyphOrder())
        lig_map = build_ligature_map(font)
        resolved, missing = resolve(ICONS, lig_map, glyph_set)
        if missing:
            print("未解析到的图标名（可能拼错或该版本字体已改名）：", missing)
            return 1

        # 1) 实例化成静态字体（默认轴值：outlined / 400 / 24 / 0）
        instancer.instantiateVariableFont(
            font, {"FILL": 0, "wght": 400, "opsz": 24, "GRAD": 0}, inplace=True
        )

        # 2) 精简 GSUB 到只含本项目 45 个连字
        build_minimal_gsub(font, lig_map)

        # 3) 子集：保留图标字形 + 组成字母字形，闭包只跟走精简后的 GSUB
        keep = sorted(
            set(resolved.values())
            | set(char_to_glyph(c) for c in "abcdefghijklmnopqrstuvwxyz0123456789_")
        )
        options = subset.Options()
        options.glyph_names = True
        options.layout_features = ["rlig"]
        options.name_IDs = ["*"]
        options.name_languages = ["*"]
        options.notdef_glyph = True
        options.notdef_outline = False
        options.recommended_glyphs = True
        options.recalc_bounds = True
        options.recalc_timestamp = False
        options.canonical_order = True
        options.flavor = "woff2"
        subsetter = subset.Subsetter(options=options)
        subsetter.populate(glyphs=keep)
        subsetter.subset(font)

        out = os.path.join(td, "subset.woff2")
        font.flavor = "woff2"
        font.save(out)
        data = open(out, "rb").read()
        print(f"图标 {len(resolved)} 个，保留字形 {len(keep)} 个，子集 {len(data) / 1024:.1f} KB")

        out_dir = os.path.join(repo, "apps/douyin-web/public/fonts")
        os.makedirs(out_dir, exist_ok=True)
        dest = os.path.join(out_dir, "material-symbols-outlined.woff2")
        with open(dest, "wb") as fh:
            fh.write(data)
        print(f"已写入 -> {dest}")

        b64_path = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                "material-symbols-outlined.base64.txt")
        with open(b64_path, "w") as fh:
            fh.write(base64.b64encode(data).decode())
        print(f"base64 已写入 -> {b64_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
