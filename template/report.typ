// push 战报模板。数据由 push_bot 生成，通过 `--input data=<json 绝对路径>` 传入。
// 本地预览：typst compile --root / --font-path fonts --ignore-system-fonts \
//   --input data=$PWD/template/sample.json template/report.typ out.png

#import "@preview/cmarker:0.1.10"

#let data = json(sys.inputs.at("data"))

// 夜间配色。bg-color 与背景图底部颜色一致，页面比背景图长时能无缝衔接
#let bg-color = rgb("#0B0E15")
#let ink = rgb("#E6EDF3")
#let muted = rgb("#A0A8B3")
#let accent = rgb("#3FD0C2")
#let rule = rgb("#1F2633")
#let star = rgb("#C9D1D9")
#let code-bg = rgb("#1A202B")
#let quote-bar = rgb("#2D3545")

#let page-width = 720pt
#let margin = (x: 52pt, top: 64pt, bottom: 44pt)

#set page(width: page-width, height: auto, margin: margin, fill: bg-color)
// 不保留中英文之间的自动空格，昵称要原样显示
// 字体与 docs（Outline）一致：西文 Inter，中文回退思源黑体
#set text(font: ("Inter", "Noto Sans CJK SC", "Noto Color Emoji"), size: 13pt, fill: ink, lang: "zh", region: "cn", cjk-latin-spacing: none)
#set par(leading: 0.75em, spacing: 0.9em, justify: false)

#let mono(body, ..args) = text(font: ("JetBrains Mono", "Noto Sans CJK SC"), ..args, body)
#let hr() = line(length: 100%, stroke: 0.6pt + rule)

// 文档里的图片：不超过正文宽度，高度不超过 320pt，保持比例
#let fitted-image(source, alt: none, ..args) = layout(size => {
  let natural = measure(image(source))
  let max-h = 320pt
  let w = calc.min(size.width, natural.width, max-h * (natural.width / natural.height))
  block(above: 10pt, below: 10pt, box(radius: 4pt, clip: true, image(source, alt: alt, width: w)))
})

// 按 docs 的样式渲染成员写的 markdown。raw-typst 必须关掉，否则文档里的注释能注入 Typst 代码
#let md(source) = {
  show heading: it => {
    let size = ("1": 1.45em, "2": 1.3em, "3": 1.15em).at(str(it.level), default: 1em)
    block(above: 1.1em, below: 0.6em, text(size: size, weight: "bold", it.body))
  }
  set list(indent: 2pt, body-indent: 8pt, marker: text(fill: muted)[•])
  set enum(indent: 2pt, body-indent: 8pt)
  show link: set text(fill: accent)
  show quote.where(block: true): it => block(
    stroke: (left: 3pt + quote-bar), inset: (left: 12pt, y: 2pt), text(fill: muted, it.body),
  )
  show raw: set text(font: ("JetBrains Mono", "Noto Sans CJK SC"), size: 0.86em)
  show raw.where(block: false): it => box(fill: code-bg, inset: (x: 4pt), outset: (y: 3pt), radius: 3pt, it)
  // 代码块去掉语法高亮：默认配色是给浅色背景设计的
  set raw(theme: none)
  show raw.where(block: true): it => block(fill: code-bg, inset: 12pt, radius: 6pt, width: 100%, it)
  cmarker.render(
    source,
    smart-punctuation: false,
    raw-typst: false,
    set-document-title: false,
    task-list-marker: checked => box(
      width: 0.85em, height: 0.85em, baseline: 0.12em, radius: 2pt,
      fill: if checked { accent } else { none }, stroke: if checked { none } else { 1pt + muted },
      if checked { align(center + horizon, text(size: 0.7em, fill: bg-color, weight: "bold")[✓]) },
    ),
    scope: (image: fitted-image),
  )
}

#let avatar(e, size: 36pt) = {
  if e.avatar != none {
    box(width: size, height: size, radius: 50%, clip: true, image(e.avatar, width: size, height: size, fit: "cover"))
  } else {
    box(width: size, height: size, radius: 50%, fill: rgb(e.color),
      align(center + horizon, text(fill: white, weight: "bold", size: size * 0.42, e.initial)))
  }
}

// 北斗七星，高亮 Merak（天璇），即战队名
#let dipper(w: 54pt, h: 24pt) = {
  let stars = (
    alkaid: (0.00, 0.62), mizar: (0.22, 0.36), alioth: (0.40, 0.40), megrez: (0.58, 0.46),
    phecda: (0.63, 0.92), merak: (0.94, 0.98), dubhe: (1.00, 0.42),
  )
  let pt(name) = (stars.at(name).at(0) * w, stars.at(name).at(1) * h)
  // 一笔连成勺形：勺柄 → 天权 → 天玑 → 天璇 → 天枢，勺口不封闭
  let edges = (
    ("alkaid", "mizar"), ("mizar", "alioth"), ("alioth", "megrez"),
    ("megrez", "phecda"), ("phecda", "merak"), ("merak", "dubhe"),
  )
  box(width: w, height: h, {
    for (a, b) in edges {
      place(line(start: pt(a), end: pt(b), stroke: 0.6pt + muted.darken(30%)))
    }
    for (name, _) in stars {
      let team = name == "merak"
      let r = if team { 2.4pt } else { 1.5pt }
      let (x, y) = pt(name)
      place(dx: x - r, dy: y - r, circle(radius: r, fill: if team { accent } else { star }))
    }
  })
}

// 背景：page 的 background 会把内容垂直居中，所以放在正文最前面，贴住页面左上角
#place(top + left, dx: -margin.x, dy: -margin.top, image(data.bg, width: page-width))

// 标题
#grid(
  columns: (1fr, auto),
  align: (left + bottom, right + bottom),
  stack(
    spacing: 10pt,
    box(dipper()) + h(10pt) + box(baseline: -8pt, mono(size: 9pt, fill: muted, tracking: 2pt)[MERAK]),
    v(8pt),
    text(size: 30pt, weight: "bold")[push 战报],
    mono(size: 12pt, fill: muted, data.range),
  ),
  stack(
    spacing: 8pt,
    text(size: 30pt, weight: "bold", fill: accent, str(data.updated)) + text(size: 16pt, fill: muted, " / " + str(data.total)),
    align(right, text(size: 10pt, fill: muted)[已更新]),
  ),
)
#v(20pt)
#hr()

// 有更新的成员
#if data.total == 0 {
  v(24pt)
  text(fill: muted)[成员名单为空。]
  v(8pt)
} else if data.entries.len() == 0 {
  v(24pt)
  text(fill: muted)[这段时间没有人更新。]
  v(8pt)
}
#for e in data.entries {
  v(18pt)
  grid(
    columns: (36pt, 1fr),
    column-gutter: 14pt,
    avatar(e),
    {
      grid(
        columns: (1fr, auto),
        align: (left + horizon, right + horizon),
        text(size: 15pt, weight: "bold", e.name),
        {
          mono(size: 11pt, fill: muted, e.time)
          h(10pt)
          mono(size: 11pt, fill: accent, weight: "bold", "+" + str(e.added))
          if e.removed > 0 { h(6pt); mono(size: 11pt, fill: muted, "-" + str(e.removed)) }
        },
      )
      v(4pt)
      md(e.markdown)
    },
  )
  v(18pt)
  hr()
}

// 未更新
#if data.idle.len() > 0 {
  v(18pt)
  text(size: 12pt, weight: "bold")[未更新]
  text(size: 12pt, fill: muted)[ · #data.idle.len()]
  v(8pt, weak: true)
  // 每个名字放进 box，换行时不会把名字拆开
  text(fill: muted, data.idle.map(n => box(n)).join([ #h(4pt)·#h(4pt) ]))
}

#v(32pt)
#align(center, mono(size: 8.5pt, fill: muted, data.footer))
