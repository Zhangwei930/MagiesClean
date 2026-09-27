#!/usr/bin/env python3
"""生成 Magies Clean 应用图标源图（1024×1024 PNG），纯 Python，无第三方依赖。

圆角方形（靛蓝→天蓝渐变）+ 白色水滴 + 斜向“擦除”高光 + 四角星。
用法：python3 scripts/gen_icon.py apps/desktop/src-tauri/icons/icon-source.png
然后：npx tauri icon apps/desktop/src-tauri/icons/icon-source.png
"""
import math, struct, sys, zlib

S = 1024

def clamp(v, a=0.0, b=1.0):
    return a if v < a else b if v > b else v

def sd_round_rect(x, y, cx, cy, hw, hh, r):
    qx = abs(x - cx) - hw + r
    qy = abs(y - cy) - hh + r
    return math.hypot(max(qx, 0), max(qy, 0)) + min(max(qx, qy), 0) - r

def sd_drop(x, y):
    # 水滴：底部圆 + 顶部尖角（圆与切线构成的锥）
    cx, cy, r = 0.5, 0.585, 0.205
    tip = (0.5, 0.2)
    d_circle = math.hypot(x - cx, y - cy) - r
    # 锥形：到两条切线的有符号距离
    dist_c = cy - tip[1]
    half = math.asin(r / dist_c)
    ang = math.atan2(x - tip[0], y - tip[1])
    rho = math.hypot(x - tip[0], y - tip[1])
    # 在锥内部 → 距离为到侧边的负距离
    along = rho * math.cos(abs(ang))
    side = rho * math.sin(abs(ang) - half)
    tangent_len = math.sqrt(dist_c * dist_c - r * r)
    cone = side if (y > tip[1] and along < tangent_len * math.cos(half)) else 1e9
    if y <= tip[1]:
        cone = math.hypot(x - tip[0], y - tip[1])
    return min(d_circle, cone)

def sd_star(x, y, cx, cy, s):
    # 四角星：|dx|^0.5 + |dy|^0.5 形状
    dx, dy = abs(x - cx) / s, abs(y - cy) / s
    v = math.sqrt(dx) + math.sqrt(dy)
    return (v - 1.0) * s * 0.5

def cover(d, px):
    return clamp(0.5 - d / px)

def main(out):
    px = 1.0 / S
    rows = []
    c0 = (91, 76, 246)   # 靛紫
    c1 = (46, 144, 250)  # 天蓝
    for j in range(S):
        row = bytearray([0])
        y = (j + 0.5) / S
        for i in range(S):
            x = (i + 0.5) / S
            d_bg = sd_round_rect(x, y, 0.5, 0.5, 0.41, 0.41, 0.19)
            a_bg = cover(d_bg, px)
            if a_bg <= 0:
                row += b"\x00\x00\x00\x00"
                continue
            t = clamp((x * 0.6 + y * 0.8) / 1.3)
            r = c0[0] + (c1[0] - c0[0]) * t
            g = c0[1] + (c1[1] - c0[1]) * t
            b = c0[2] + (c1[2] - c0[2]) * t
            # 顶部柔光
            glow = clamp(1.0 - math.hypot(x - 0.3, y - 0.18) / 0.55) * 0.18
            r, g, b = r + (255 - r) * glow, g + (255 - g) * glow, b + (255 - b) * glow
            # 水滴
            a_drop = cover(sd_drop(x, y), px) * 0.96
            # 斜向擦除带：在水滴内部挖出一条渐隐的高光带
            band = abs((x - 0.5) * 0.7071 + (y - 0.62) * 0.7071 + 0.02)
            cut = clamp((0.035 - band) / 0.01) * 0.55
            a_drop *= (1.0 - cut)
            r, g, b = r + (255 - r) * a_drop, g + (255 - g) * a_drop, b + (255 - b) * a_drop
            a_star = cover(sd_star(x, y, 0.72, 0.3, 0.085), px)
            r, g, b = r + (255 - r) * a_star, g + (255 - g) * a_star, b + (255 - b) * a_star
            row += bytes((int(r), int(g), int(b), int(a_bg * 255)))
        rows.append(bytes(row))
    raw = b"".join(rows)
    def chunk(t, d):
        c = struct.pack(">I", len(d)) + t + d
        return c + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", S, S, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")
    open(out, "wb").write(png)

if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "icon-source.png")
