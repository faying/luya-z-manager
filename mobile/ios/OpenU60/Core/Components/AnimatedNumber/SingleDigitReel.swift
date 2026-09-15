import SwiftUI

struct SingleDigitReel: View {
    let digit: Int
    let font: Font
    let textColor: Color
    let isAnimated: Bool
    let animationDuration: Double

    private static let totalSets = 7
    private static let digitsPerSet = 10
    private var totalSlots: Int { Self.totalSets * Self.digitsPerSet }
    private static var middleSetStart: Int { (totalSets / 2) * digitsPerSet }

    @State private var cumulativePosition: Int

    init(digit: Int, font: Font, textColor: Color, isAnimated: Bool, animationDuration: Double) {
        self.digit = digit
        self.font = font
        self.textColor = textColor
        self.isAnimated = isAnimated
        self.animationDuration = animationDuration

        _cumulativePosition = State(initialValue: Self.middleSetStart + digit)
    }

    var body: some View {
        Text("8")
            .font(font.monospacedDigit())
            .foregroundStyle(.clear)
            .overlay {
                GeometryReader { proxy in
                    let slotHeight = proxy.size.height
                    VStack(spacing: 0) {
                        ForEach(0..<totalSlots, id: \.self) { index in
                            Text("\(index % Self.digitsPerSet)")
                                .font(font.monospacedDigit())
                                .foregroundStyle(textColor)
                                .frame(width: proxy.size.width, height: slotHeight)
                        }
                    }
                    .offset(y: -CGFloat(cumulativePosition) * slotHeight)
                }
            }
            .clipped()
            .onChange(of: digit) { oldValue, newValue in
                let delta = Self.shortestDelta(from: oldValue, to: newValue)
                if isAnimated {
                    withAnimation(.easeInOut(duration: animationDuration)) {
                        cumulativePosition += delta
                    } completion: {
                        snapToMiddle(newValue)
                    }
                } else {
                    cumulativePosition += delta
                    snapToMiddle(newValue)
                }
            }
    }

    /// Computes the shortest path on the mod-10 ring.
    /// Positive = forward (rolling down), negative = backward (rolling up).
    private static func shortestDelta(from: Int, to: Int) -> Int {
        let forward = (to - from + 10) % 10   // e.g. 9→0: (0-9+10)%10 = 1
        let backward = forward - 10            // e.g. 9→0: 1-10 = -9
        return abs(forward) <= abs(backward) ? forward : backward
    }

    /// Snap back to the middle set without animation to prevent unbounded offset growth.
    private func snapToMiddle(_ digit: Int) {
        let targetPosition = Self.middleSetStart + digit
        if cumulativePosition != targetPosition {
            var transaction = Transaction()
            transaction.disablesAnimations = true
            withTransaction(transaction) {
                cumulativePosition = targetPosition
            }
        }
    }
}
