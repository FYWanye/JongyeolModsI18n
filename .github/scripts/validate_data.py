#!/usr/bin/env python3
"""校验 data/ 下的汉化表：JSON 合法、键值均为字符串、data 与 Resources 保持一致。

build.ps1 会把 data/ 逐字节同步进 Resources/（作为 DLL 内嵌资源），
因此两者必须完全一致；若不一致，说明有人手改过 Resources/，应回到 data/ 修改后重跑 build.ps1。
"""

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DATA = ROOT / "data"
RESOURCES = ROOT / "Resources"

errors = []


def validate(path: Path) -> None:
    try:
        obj = json.loads(path.read_text(encoding="utf-8-sig"))
    except Exception as exc:
        errors.append(f"{path.relative_to(ROOT)}: JSON 解析失败: {exc}")
        return
    if not isinstance(obj, dict):
        errors.append(f"{path.relative_to(ROOT)}: 顶层应为 JSON 对象，实际是 {type(obj).__name__}")
        return
    for key, value in obj.items():
        if not isinstance(key, str) or not isinstance(value, str):
            errors.append(
                f"{path.relative_to(ROOT)}: 键值必须都是字符串，发现 {key!r} -> {type(value).__name__}"
            )


data_files = sorted(DATA.glob("*.json"))
if not data_files:
    errors.append("data/ 目录下没有任何 JSON 汉化表")

for path in data_files:
    validate(path)
    res_path = RESOURCES / path.name
    if not res_path.is_file():
        errors.append(f"{path.relative_to(ROOT)}: 缺少对应的 {res_path.relative_to(ROOT)}（构建时由 data 同步）")
    elif path.read_bytes() != res_path.read_bytes():
        errors.append(f"{path.name}: data/ 与 Resources/ 内容不一致，请修改 data/ 后运行 build.ps1 重新同步")

if errors:
    print("汉化表校验失败：", file=sys.stderr)
    for error in errors:
        print(f"  - {error}", file=sys.stderr)
    sys.exit(1)

print(f"汉化表校验通过：{len(data_files)} 个文件，data/ 与 Resources/ 保持一致。")
