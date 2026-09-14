# -*- coding: utf-8 -*-
# 从 docs/team/reports/web-blog.md 提取 CSDN 版素材: 标题/摘要/标签 + 正文按图片占位切 6 段
import re, os

root = r"C:\Users\liuyu\Desktop\WorkPlace\serialhub"
src = open(os.path.join(root, "docs/team/reports/web-blog.md"), encoding="utf-8").read()
lines = src.splitlines()

start = next(i for i, l in enumerate(lines) if l.startswith("# 串口调试提效"))
end = next(i for i, l in enumerate(lines) if l.startswith("## 第二部分"))
sep = max(i for i in range(start, end) if lines[i].strip() == "---")
body_lines = lines[start:sep]
while body_lines and not body_lines[-1].strip():
    body_lines.pop()

title = next(l for l in lines if l.startswith("- **主标题**")).split("**: ", 1)[1].strip()
abstract = next(l for l in lines if l.lstrip().startswith("> SerialHub")).lstrip(" >").strip()
tagline = next(l for l in lines if l.startswith("- **标签建议")).split("**: ", 1)[1].strip()
tags = [t.strip() for t in tagline.split("·") if t.strip()]

outdir = os.path.join(root, "build", "csdn-publish")
os.makedirs(outdir, exist_ok=True)

img_re = re.compile(r"^!\[[^\]]*\]\(docs/images/([^)]+)\)\s*$")
segs, imgs, cur = [], [], []
for l in body_lines:
    m = img_re.match(l)
    if m:
        segs.append(cur)
        imgs.append(m.group(1))
        cur = []
    else:
        cur.append(l)
segs.append(cur)
# H1 标题行不入正文 (CSDN 标题单独填)
segs[0] = [l for l in segs[0] if not l.startswith("# 串口调试提效")]

for n, s in enumerate(segs, 1):
    with open(os.path.join(outdir, "seg%d.md" % n), "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(s))

# 自检: seg1..6 + 图片占位重组后应与原正文(去 H1)一致
joined = []
for i, s in enumerate(segs):
    joined.extend(s)
    if i < len(imgs):
        joined.append("![X](IMG_%d)" % (i + 1))
recon = "\n".join(joined).strip()
expect = "\n".join(l for l in "\n".join(body_lines).splitlines()
                   if not l.startswith("# 串口调试提效")).strip()
print("RECON:", "MATCH" if recon == expect else "MISMATCH")
print("IMGS:", imgs)
print("TAGS:", tags)
print("TITLE:", title)
print("SEG_CHARS:", [len("\n".join(s)) for s in segs])
with open(os.path.join(outdir, "title.txt"), "w", encoding="utf-8") as f:
    f.write(title)
with open(os.path.join(outdir, "abstract.txt"), "w", encoding="utf-8") as f:
    f.write(abstract)
with open(os.path.join(outdir, "tags.txt"), "w", encoding="utf-8") as f:
    f.write("\n".join(tags))
