# -*- coding: utf-8 -*-
"""在播主播名单（历史任意一天音浪>0）——字辈汇总 + 明细 两个 sheet"""
try:
    import openpyxl
except ImportError:
    import subprocess, sys
    subprocess.check_call([sys.executable, "-m", "pip", "install", "--quiet", "openpyxl>=3.1.0"])
    import openpyxl

from openpyxl.styles import Font, PatternFill, Alignment, Border, Side
from openpyxl.utils import get_column_letter
from datetime import date

def xl_color(css_hex: str) -> str:
    value = css_hex.removeprefix("#").upper()
    if len(value) != 6:
        raise ValueError(f"Expected #RRGGBB, got: {css_hex}")
    return "FF" + value

XL_PRIMARY = xl_color("#4472C4")
XL_BORDER = xl_color("#BFBFBF")
XL_LIGHT = xl_color("#D9E2F3")
XL_GRAY_FONT = xl_color("#808080")

thin_side = Side(style="thin", color=XL_BORDER)
BORDER_THIN = Border(left=thin_side, right=thin_side, top=thin_side, bottom=thin_side)

# ============ 数据：person + daily_metric（历史任意一天 wave>0），来源 douyin_dev 2026-10-08 ============
# (name, gender, master, max_wave, total_wave, days, first_day, last_day, status)
ROWS = [
    ("三宝", "female", None, 208611, 2377625, 62, "2026-05-01", "2026-10-05", "active"),
    ("凤九", "male", None, 167905, 453326, 15, "2026-09-21", "2026-10-05", "active"),
    ("凤夏", "male", None, 16886, 43144, 15, "2026-09-21", "2026-10-05", "active"),
    ("凤瑶", "male", None, 28133, 99327, 15, "2026-09-21", "2026-10-05", "active"),
    ("南方楠", "male", "鹏先生", 863205, 2836913, 72, "2026-05-02", "2026-10-05", "active"),
    ("啸升", "male", "狼泽", 21107, 31331, 14, "2026-09-21", "2026-10-05", "active"),
    ("啸墨", "male", "狼腾", 30835, 51819, 32, "2026-06-07", "2026-10-01", "active"),
    ("啸宇", "male", "狼俊", 78527, 545304, 79, "2026-05-01", "2026-10-05", "active"),
    ("啸安", "male", "狼兴", 251910, 650748, 71, "2026-05-02", "2026-10-05", "active"),
    ("啸寻", "male", None, 31462, 87729, 15, "2026-09-21", "2026-10-05", "active"),
    ("啸帆", "male", "狼兴", 270092, 436979, 13, "2026-09-21", "2026-10-05", "active"),
    ("啸弘", "male", "狼泽", 17876, 45097, 15, "2026-09-21", "2026-10-05", "active"),
    ("啸强", "male", "狼俊", 56218, 214155, 75, "2026-05-01", "2026-10-05", "active"),
    ("啸恒", "male", "狼小宝", 84566, 241505, 15, "2026-09-21", "2026-10-05", "active"),
    ("啸昭", "male", None, 23126, 60111, 15, "2026-09-21", "2026-10-05", "active"),
    ("啸森", "male", "狼俊", 228952, 587695, 78, "2026-05-01", "2026-10-05", "active"),
    ("啸沐", "male", "狼征", 194, 1619, 24, "2026-06-12", "2026-10-01", "active"),
    ("啸泽", "male", "狼兴", 86046, 344769, 76, "2026-05-01", "2026-10-05", "active"),
    ("啸燃", "male", "狼腾", 26032, 70334, 11, "2026-09-21", "2026-10-04", "active"),
    ("啸辰", "male", "狼兴", 179014, 717875, 78, "2026-05-01", "2026-10-05", "active"),
    ("啸钰", "male", "狼腾", 13999, 72393, 46, "2026-06-01", "2026-10-05", "active"),
    ("啸阳", "male", "狼泽", 62244, 177831, 31, "2026-06-15", "2026-10-04", "active"),
    ("啸鸣", "male", None, 1103, 2774, 11, "2026-09-21", "2026-10-05", "active"),
    ("晓倩", "female", None, 88000, 716716, 69, "2026-05-01", "2026-10-05", "active"),
    ("晓妍", "female", None, 226493, 983191, 80, "2026-05-01", "2026-10-05", "active"),
    ("晓婉", "female", None, 170498, 782679, 72, "2026-05-06", "2026-10-05", "active"),
    ("晓宝", "female", None, 42142, 305867, 65, "2026-05-06", "2026-10-01", "active"),
    ("晓暖", "female", None, 109624, 525184, 60, "2026-05-01", "2026-10-04", "active"),
    ("晓楠", "female", None, 83289, 776872, 68, "2026-05-01", "2026-10-05", "active"),
    ("晓涵", "female", None, 205232, 1261809, 74, "2026-05-01", "2026-10-05", "active"),
    ("晓爱", "female", None, 601029, 1557868, 63, "2026-05-02", "2026-10-05", "active"),
    ("晓雅", "female", None, 132640, 793673, 62, "2026-05-06", "2026-10-05", "active"),
    ("晓雨", "female", None, 193479, 762052, 75, "2026-05-01", "2026-10-05", "active"),
    ("晓静", "female", None, 150895, 1386630, 79, "2026-05-01", "2026-10-05", "active"),
    ("浩冬", "male", "鹏先生", 630870, 2467600, 71, "2026-05-04", "2026-10-05", "active"),
    ("浩启", "male", "鹏先生", 184264, 749095, 62, "2026-05-01", "2026-10-01", "active"),
    ("浩哲", "male", "鹏先生", 510571, 1267570, 78, "2026-05-01", "2026-10-05", "active"),
    ("浩坤", "male", "鹏先生", 26960, 43917, 48, "2026-05-06", "2026-10-01", "active"),
    ("浩宁", "male", None, 47907, 118120, 15, "2026-09-21", "2026-10-05", "active"),
    ("浩延", "male", "鹏先生", 109631, 585956, 57, "2026-05-04", "2026-10-05", "active"),
    ("浩月", "male", "鹏先生", 104751, 337277, 73, "2026-05-01", "2026-10-05", "active"),
    ("浩杰", "male", "鹏先生", 69232, 311957, 70, "2026-05-02", "2026-10-05", "active"),
    ("浩森", "male", "鹏先生", 92949, 220054, 72, "2026-05-05", "2026-10-05", "active"),
    ("浩楠", "male", "鹏先生", 134368, 176395, 12, "2026-09-21", "2026-10-04", "active"),
    ("浩沐", "male", "鹏先生", 120632, 212828, 15, "2026-09-21", "2026-10-05", "active"),
    ("浩泽", "male", "鹏先生", 81648, 235669, 67, "2026-05-06", "2026-10-01", "active"),
    ("浩然", "male", "鹏先生", 65499, 344439, 72, "2026-05-02", "2026-10-05", "active"),
    ("浩玟", "male", "鹏先生", 173047, 959984, 70, "2026-05-03", "2026-10-05", "active"),
    ("浩艺", "male", "鹏先生", 56149, 612984, 61, "2026-05-03", "2026-10-05", "active"),
    ("浩辰", "male", "鹏先生", 519823, 1265065, 67, "2026-05-03", "2026-10-05", "active"),
    ("浩运", "male", "鹏先生", 75167, 509283, 73, "2026-05-03", "2026-10-01", "active"),
    ("浩鑫", "male", None, 39725, 39884, 7, "2026-09-21", "2026-10-01", "active"),
    ("浩阳", "male", "鹏先生", 53906, 946699, 76, "2026-05-01", "2026-10-05", "active"),
    ("浩雨", "male", "鹏先生", 302622, 1547171, 67, "2026-05-01", "2026-10-05", "active"),
    ("浩鸣", "male", "鹏先生", 562959, 2763156, 71, "2026-05-01", "2026-10-05", "active"),
    ("浩龙", "male", "鹏先生", 1406169, 3189147, 66, "2026-05-03", "2026-10-05", "active"),
    ("狼九", "male", "浩冬", 233372, 371522, 48, "2026-06-01", "2026-10-05", "active"),
    ("狼仔", "male", "南方楠", 221785, 1059178, 58, "2026-05-01", "2026-10-01", "active"),
    ("狼佑", "male", "浩龙", 99670, 161895, 15, "2026-09-21", "2026-10-05", "active"),
    ("狼俊", "male", "浩雨", 139116, 1149398, 78, "2026-05-01", "2026-10-05", "active"),
    ("狼兴", "male", "浩雨", 596292, 2750937, 77, "2026-05-01", "2026-10-05", "active"),
    ("狼凯", "male", "南方楠", 32947, 281333, 63, "2026-05-02", "2026-10-01", "active"),
    ("狼卓", "male", "浩辰", 5643, 5982, 7, "2026-09-21", "2026-10-01", "active"),
    ("狼博", "male", "浩龙", 50271, 823087, 69, "2026-05-01", "2026-10-01", "active"),
    ("狼哲", "male", "浩辰", 179459, 682344, 80, "2026-05-01", "2026-10-05", "active"),
    ("狼安", "male", "浩辰", 6244, 72475, 65, "2026-05-01", "2026-10-01", "active"),
    ("狼小宝", "male", "浩龙", 309261, 1807324, 80, "2026-05-01", "2026-10-05", "active"),
    ("狼岳", "male", "浩龙", 121798, 424498, 80, "2026-05-01", "2026-10-05", "active"),
    ("狼帅", "male", "浩玟", 13717, 104161, 72, "2026-05-01", "2026-10-02", "active"),
    ("狼彬", "male", "浩玟", 9190, 10728, 10, "2026-09-21", "2026-10-04", "active"),
    ("狼影", "male", "浩冬", 253446, 639248, 62, "2026-05-01", "2026-10-04", "active"),
    ("狼征", "male", "浩雨", 43103, 186910, 63, "2026-05-03", "2026-10-01", "active"),
    ("狼旭", "male", "浩雨", 45308, 139095, 59, "2026-05-02", "2026-10-02", "active"),
    ("狼明", "male", "浩龙", 170697, 759397, 71, "2026-05-01", "2026-10-05", "active"),
    ("狼森", "male", None, 12028, 125613, 47, "2026-05-01", "2026-06-30", "left"),
    ("狼泽", "male", "浩杰", 89859, 630959, 67, "2026-05-02", "2026-10-05", "active"),
    ("狼浩", "male", "南方楠", 216, 216, 4, "2026-09-21", "2026-10-01", "active"),
    ("狼澈", "male", "浩雨", 480345, 3194525, 77, "2026-05-01", "2026-10-05", "active"),
    ("狼瑞", "male", "浩辰", 21463, 162712, 38, "2026-05-02", "2026-10-04", "active"),
    ("狼耀", "male", None, 43703, 141529, 15, "2026-09-21", "2026-10-05", "active"),
    ("狼腾", "male", "南方楠", 202754, 2364008, 78, "2026-05-01", "2026-10-05", "active"),
    ("狼航", "male", "浩辰", 2432, 2594, 7, "2026-05-14", "2026-10-01", "active"),
    ("狼艺", "male", "浩雨", 43286, 394868, 78, "2026-05-01", "2026-10-05", "active"),
    ("狼裕", "male", "浩冬", 2697, 43406, 59, "2026-05-04", "2026-10-01", "active"),
    ("狼赫", "male", "浩雨", 146597, 1074742, 77, "2026-05-01", "2026-10-05", "active"),
    ("狼轩", "male", "南方楠", 650751, 1135906, 47, "2026-05-03", "2026-10-01", "active"),
    ("狼辉", "male", "南方楠", 125330, 684976, 74, "2026-05-01", "2026-10-05", "active"),
    ("狼辰", "male", "浩杰", 192014, 1145003, 53, "2026-05-01", "2026-10-05", "active"),
    ("狼途", "male", "浩辰", 12720, 30742, 19, "2026-06-29", "2026-10-05", "active"),
    ("狼鑫", "male", None, 10932, 16430, 15, "2026-09-21", "2026-10-05", "active"),
    ("狼雨", "male", "浩冬", 90944, 141884, 15, "2026-09-21", "2026-10-05", "active"),
    ("狼霆", "male", "浩辰", 34194, 215960, 69, "2026-05-01", "2026-10-01", "active"),
    ("狼驰", "male", None, 14090, 23204, 12, "2026-09-21", "2026-10-04", "active"),
    ("玖久", "male", None, 3044, 13746, 14, "2026-09-21", "2026-10-05", "active"),
    ("玖依", "male", "浩冬", 4672, 4672, 4, "2026-09-21", "2026-10-01", "active"),
    ("玖妹", "male", "狼腾", 82268, 389376, 48, "2026-06-01", "2026-10-05", "active"),
    ("玖安", "male", None, 14604, 37075, 15, "2026-09-21", "2026-10-05", "active"),
    ("玖忆", "male", None, 1004, 5695, 14, "2026-09-21", "2026-10-05", "active"),
    ("玖柒", "male", "浩龙", 197911, 1023550, 80, "2026-05-01", "2026-10-05", "active"),
    ("玖玉", "male", "狼轩", 1671865, 2424244, 67, "2026-05-06", "2026-10-05", "active"),
    ("玖玥", "male", "浩龙", 394181, 2045749, 76, "2026-05-02", "2026-10-05", "active"),
    ("玖豆", "male", "南方楠", 96765, 158302, 14, "2026-09-21", "2026-10-04", "active"),
    ("玖雪", "male", "浩辰", 176534, 603634, 80, "2026-05-01", "2026-10-05", "active"),
    ("玖颜", "male", "浩龙", 11515, 64517, 38, "2026-05-01", "2026-10-01", "active"),
    ("鹏先生", "male", None, 225237, 659498, 14, "2026-09-21", "2026-10-05", "active"),
]

def clan_of(name: str) -> str:
    return name[0] if name[0] in ("浩", "啸", "狼", "玖", "晓", "凤") else "其他"

CLAN_ORDER = ["浩", "啸", "狼", "玖", "晓", "凤", "其他"]
# 组内按历史累计音浪降序
SORTED_ROWS = sorted(
    ROWS,
    key=lambda r: (CLAN_ORDER.index(clan_of(r[0])), -r[4], r[0]),
)
N = len(SORTED_ROWS)

GENDER = {"male": "男团", "female": "女团"}
STATUS = {"active": "在册", "left": "已离团"}

wb = openpyxl.Workbook()
wb.properties.title = "在播主播名单"

# ==================== Sheet1: 字辈汇总（锚点：1标题 / 2表头 / 3-9数据 / 10合计） ====================
# 静态快照表：合计/汇总在 Python 里算好写值（本机无公式重算引擎，公式在预览器中会显示空白）
ws_sum = wb.active
ws_sum.title = "字辈汇总"

clans = CLAN_ORDER  # 7 组 → 行 3..9
clan_stats = []
for clan in clans:
    grp = [r for r in SORTED_ROWS if clan_of(r[0]) == clan]
    clan_stats.append((clan, len(grp), sum(r[4] for r in grp)))
GRAND_N, GRAND_TOTAL = len(SORTED_ROWS), sum(r[4] for r in SORTED_ROWS)

ws_sum.merge_cells("A1:E1")
ws_sum["A1"] = "在播主播字辈分布（历史任意一天音浪 > 0 即算在播）"
ws_sum["A1"].font = Font(bold=True, size=14, color="FF1F3864")
ws_sum["A1"].alignment = Alignment(horizontal="center", vertical="center")
ws_sum.row_dimensions[1].height = 26

headers = ["字辈", "人数", "占比", "累计音浪合计", "人均累计音浪"]
for i, h in enumerate(headers, start=1):
    c = ws_sum.cell(row=2, column=i, value=h)
    c.font = Font(bold=True, color=xl_color("#FFFFFF"))
    c.fill = PatternFill("solid", fgColor=XL_PRIMARY)
    c.alignment = Alignment(horizontal="center", vertical="center")
    c.border = BORDER_THIN

for idx, (clan, cnt, total) in enumerate(clan_stats):
    r = 3 + idx
    ws_sum.cell(row=r, column=1, value=clan if clan != "其他" else "其他（南方楠/三宝/鹏先生）")
    ws_sum.cell(row=r, column=2, value=cnt)
    ws_sum.cell(row=r, column=3, value=(cnt / GRAND_N) if GRAND_N else None)
    ws_sum.cell(row=r, column=4, value=total)
    ws_sum.cell(row=r, column=5, value=round(total / cnt) if cnt else None)
    for col in range(1, 6):
        c = ws_sum.cell(row=r, column=col)
        c.border = BORDER_THIN
        if col == 2:
            c.alignment = Alignment(horizontal="center")
            c.number_format = "0"
        elif col == 3:
            c.alignment = Alignment(horizontal="right")
            c.number_format = "0.0%"
        elif col in (4, 5):
            c.alignment = Alignment(horizontal="right")
            c.number_format = "#,##0"

ws_sum["A10"] = "合计"
ws_sum["B10"] = GRAND_N
ws_sum["C10"] = 1
ws_sum["D10"] = GRAND_TOTAL
ws_sum["E10"] = round(GRAND_TOTAL / GRAND_N) if GRAND_N else None
for col in range(1, 6):
    c = ws_sum.cell(row=10, column=col)
    c.font = Font(bold=True, color=xl_color("#FFFFFF"))
    c.fill = PatternFill("solid", fgColor=xl_color("#2F5597"))
    c.border = BORDER_THIN
    if col == 3:
        c.number_format = "0.0%"
        c.alignment = Alignment(horizontal="right")
    elif col in (4, 5):
        c.number_format = "#,##0"
        c.alignment = Alignment(horizontal="right")
    else:
        c.alignment = Alignment(horizontal="center")

ws_sum["A12"] = "口径：历史任意一天当日音浪 > 0 即计为在播；数据截至 2026-10-05（本地库 douyin_dev）"
ws_sum["A12"].font = Font(size=9, italic=True, color=xl_color("#808080"))

for col, w in zip("ABCDE", (30, 10, 10, 18, 16)):
    ws_sum.column_dimensions[col].width = w

# ==================== Sheet2: 在播主播（锚点：1标题 / 2表头 / 3..2+N数据 / 3+N合计） ====================
ws = wb.create_sheet("在播主播")

ws.merge_cells("A1:K1")
ws["A1"] = "在播主播名单（历史任意一天音浪 > 0）"
ws["A1"].font = Font(bold=True, size=14, color="FF1F3864")
ws["A1"].alignment = Alignment(horizontal="center", vertical="center")
ws.row_dimensions[1].height = 26

headers = ["序号", "姓名", "字辈", "团别", "师傅", "资料状态",
           "最高单日音浪", "历史累计音浪", "有数据天数", "首日", "末日"]
for i, h in enumerate(headers, start=1):
    c = ws.cell(row=2, column=i, value=h)
    c.font = Font(bold=True, color=xl_color("#FFFFFF"))
    c.fill = PatternFill("solid", fgColor=XL_PRIMARY)
    c.alignment = Alignment(horizontal="center", vertical="center")
    c.border = BORDER_THIN

DATA_START = 3
DATA_END = 2 + N  # 103 行数据 → 行 3..107
for idx, (name, gender, master, max_wave, total_wave, days, first, last, status) in enumerate(SORTED_ROWS):
    r = DATA_START + idx
    f = first and date(*map(int, first.split("-")))
    l = last and date(*map(int, last.split("-")))
    values = [idx + 1, name, clan_of(name), GENDER[gender], master or "", STATUS[status],
              max_wave, total_wave, days, f, l]
    for col, v in enumerate(values, start=1):
        c = ws.cell(row=r, column=col, value=v)
        c.border = BORDER_THIN
        if col == 1:
            c.alignment = Alignment(horizontal="center")
        elif col in (2, 3, 4, 5, 6):
            c.alignment = Alignment(horizontal="left" if col in (2, 5) else "center")
        elif col in (7, 8, 9):
            c.alignment = Alignment(horizontal="right")
            c.number_format = "#,##0"
        elif col in (10, 11):
            c.alignment = Alignment(horizontal="center")
            c.number_format = "YYYY-MM-DD"
    if idx % 2 == 1:
        for col in range(1, 12):
            ws.cell(row=r, column=col).fill = PatternFill("solid", fgColor=XL_LIGHT)

TOTAL_ROW = DATA_END + 1
max_wave_all = max(r[3] for r in SORTED_ROWS)
total_wave_all = sum(r[4] for r in SORTED_ROWS)
avg_days = round(sum(r[5] for r in SORTED_ROWS) / N)
ws.cell(row=TOTAL_ROW, column=1, value="合计")
ws.cell(row=TOTAL_ROW, column=2, value=N)
ws.cell(row=TOTAL_ROW, column=7, value=max_wave_all)
ws.cell(row=TOTAL_ROW, column=8, value=total_wave_all)
ws.cell(row=TOTAL_ROW, column=9, value=avg_days)
for col in range(1, 12):
    c = ws.cell(row=TOTAL_ROW, column=col)
    c.font = Font(bold=True, color=xl_color("#FFFFFF"))
    c.fill = PatternFill("solid", fgColor=xl_color("#2F5597"))
    c.border = BORDER_THIN
    if col in (2,):
        c.alignment = Alignment(horizontal="left")
        c.number_format = '0" 人"'
    elif col in (7, 8, 9):
        c.alignment = Alignment(horizontal="right")
        c.number_format = "#,##0"
    else:
        c.alignment = Alignment(horizontal="center")

# 条件格式：资料状态=已离团 → 灰字
from openpyxl.formatting.rule import CellIsRule
ws.conditional_formatting.add(
    f"F{DATA_START}:F{DATA_END}",
    CellIsRule(operator="equal", formula=['"已离团"'],
               font=Font(color=XL_GRAY_FONT, italic=True)),
)

ws.auto_filter.ref = f"A2:K{DATA_END}"
ws.freeze_panes = "C3"

for col, w in zip("ABCDEFGHIJK", (6, 10, 8, 8, 10, 10, 14, 14, 11, 12, 12)):
    ws.column_dimensions[col].width = w

wb.save("/Volumes/2t/it/抖音pk分组/在播主播名单.xlsx")
print(f"OK: {N} 人, 合计行={TOTAL_ROW}, 字辈分布:",
      {k: sum(1 for r in SORTED_ROWS if clan_of(r[0]) == k) for k in CLAN_ORDER})
