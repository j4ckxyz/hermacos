import SwiftUI

/// Draws text normally except for its newest glyphs, which fade and sharpen in.
///
/// `fade` holds one opacity per trailing glyph, oldest first, so the last entry belongs to the
/// last glyph on screen. Lines that are entirely settled are drawn in one call.
struct TailFade: TextRenderer {
    var fade: [Float]

    func draw(layout: Text.Layout, in context: inout GraphicsContext) {
        guard !fade.isEmpty else {
            for line in layout { context.draw(line) }
            return
        }
        var total = 0
        for line in layout {
            for run in line { total += run.count }
        }
        let firstFading = total - fade.count
        var index = 0
        for line in layout {
            var lineGlyphs = 0
            for run in line { lineGlyphs += run.count }
            if index + lineGlyphs <= firstFading {
                context.draw(line)
                index += lineGlyphs
                continue
            }
            for run in line {
                for glyph in run {
                    if index >= firstFading, index - firstFading < fade.count {
                        let opacity = Double(fade[index - firstFading])
                        var copy = context
                        copy.opacity = opacity
                        copy.addFilter(.blur(radius: (1 - opacity) * 2.5))
                        copy.draw(glyph)
                    } else {
                        context.draw(glyph)
                    }
                    index += 1
                }
            }
        }
    }
}
