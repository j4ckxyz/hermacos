import Charts
import HermesCore
import SwiftUI

/// Today's usage, measured against a limit when there is one.
///
/// Hermes has no daily limit of its own. A limit is either one the user sets here (dollars or
/// tokens per day) or, failing that, the allowance of a metered plan the server reports.
struct UsageSnapshot {
    enum Limit {
        case cost(Double)
        case tokens(Double)
        case plan(PlanUsage)
    }

    let today: DayUsage?
    let limit: Limit?

    init(summary: UsageSummary?, defaults: UserDefaults = .standard) {
        today = summary.flatMap(Self.today(in:))
        let cost = defaults.double(forKey: Preferences.dailyLimitCost)
        let tokens = defaults.double(forKey: Preferences.dailyLimitTokens)
        switch defaults.string(forKey: Preferences.dailyLimitKind) {
        case "cost" where cost > 0: limit = .cost(cost)
        case "tokens" where tokens > 0: limit = .tokens(tokens)
        default: limit = summary?.plan.map(Limit.plan)
        }
    }

    /// The server reports days in its own time zone; accept this Mac's date, then UTC's.
    static func today(in summary: UsageSummary) -> DayUsage? {
        let local = DayKey.string(for: Date(), in: .current)
        let utc = DayKey.string(for: Date(), in: .gmt)
        return summary.days.last { $0.day == local } ?? summary.days.last { $0.day == utc }
    }

    var cost: Double { today?.costUsd ?? 0 }
    /// Tokens sent and received today (cache reads are counted separately).
    var tokens: UInt64 { (today?.inputTokens ?? 0) + (today?.outputTokens ?? 0) }

    /// 0...1 (or beyond) of the limit used; nil without a limit.
    var fraction: Double? {
        switch limit {
        case .cost(let budget): cost / budget
        case .tokens(let budget): Double(tokens) / budget
        case .plan(let plan): plan.fractionUsed
        case nil: nil
        }
    }

    var limitDescription: String? {
        switch limit {
        case .cost(let budget): "of \(Self.money(budget)) daily limit"
        case .tokens(let budget): "of \(Self.count(UInt64(budget))) daily token limit"
        case .plan(let plan): "\(plan.spent) of \(plan.total) · \(plan.planName)"
        case nil: nil
        }
    }

    /// What fits next to the ring: percent of the limit, else today's cost, else tokens.
    var shortLabel: String {
        if let fraction { return fraction.formatted(.percent.precision(.fractionLength(0))) }
        if cost > 0 { return Self.money(cost) }
        return Self.count(tokens)
    }

    var tint: Color {
        guard let fraction else { return .accentColor }
        return fraction >= 1 ? .red : fraction >= 0.8 ? .orange : .green
    }

    static func money(_ value: Double) -> String {
        value.formatted(
            .currency(code: "USD").presentation(.narrow)
                .precision(value < 10 ? .fractionLength(2) : .fractionLength(0...2)))
    }

    static func count(_ value: UInt64) -> String {
        value.formatted(.number.notation(.compactName).precision(.significantDigits(1...3)))
    }
}

/// `YYYY-MM-DD` keys, the form the server reports days in.
enum DayKey {
    static func string(for date: Date, in zone: TimeZone) -> String {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = zone
        let parts = calendar.dateComponents([.year, .month, .day], from: date)
        return String(format: "%04d-%02d-%02d", parts.year ?? 0, parts.month ?? 0, parts.day ?? 0)
    }

    /// Noon of that day in this Mac's time zone, so day arithmetic never slips across midnight.
    static func date(for key: String) -> Date? {
        let fields = key.split(separator: "-").compactMap { Int($0) }
        guard fields.count == 3 else { return nil }
        return Calendar.current.date(from: DateComponents(year: fields[0], month: fields[1], day: fields[2], hour: 12))
    }
}

/// The ring itself: progress toward a limit, or the input/output split when there is none.
struct UsageRing: View {
    let snapshot: UsageSnapshot
    var lineWidth: CGFloat = 3.5

    var body: some View {
        ZStack {
            Circle().stroke(.quaternary, lineWidth: lineWidth)
            if let fraction = snapshot.fraction {
                arc(from: 0, to: min(max(fraction, 0), 1), color: snapshot.tint)
            } else if let today = snapshot.today, snapshot.tokens > 0 {
                let input = Double(today.inputTokens) / Double(snapshot.tokens)
                arc(from: 0, to: input, color: .accentColor)
                arc(from: input, to: 1, color: .accentColor.opacity(0.45))
            }
        }
        .padding(lineWidth / 2)
        .animation(.smooth(duration: 0.5), value: snapshot.fraction ?? Double(snapshot.tokens))
    }

    private func arc(from start: Double, to end: Double, color: Color) -> some View {
        Circle()
            .trim(from: start, to: end)
            .stroke(color, style: StrokeStyle(lineWidth: lineWidth, lineCap: .butt))
            .rotationEffect(.degrees(-90))
    }
}

/// Sidebar footer control: the ring with a one-word reading; click for the full picture.
struct UsageButton: View {
    @Environment(AppModel.self) private var model
    @AppStorage(Preferences.dailyLimitKind) private var limitKind = "none"
    @AppStorage(Preferences.dailyLimitCost) private var limitCost = 0.0
    @AppStorage(Preferences.dailyLimitTokens) private var limitTokens = 0.0

    var body: some View {
        @Bindable var model = model
        // Reading the stored limit here re-renders the ring when it changes.
        let _ = (limitKind, limitCost, limitTokens)
        let snapshot = UsageSnapshot(summary: model.usage)
        Button {
            model.showingUsage.toggle()
            Task { await model.refreshUsage() }
        } label: {
            HStack(spacing: 6) {
                Text(model.usage == nil ? "" : snapshot.shortLabel)
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
                    .contentTransition(.numericText())
                UsageRing(snapshot: snapshot)
                    .frame(width: 22, height: 22)
            }
            .padding(.vertical, 4)
            .padding(.leading, 6)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .help(helpText(snapshot))
        .accessibilityLabel("Usage today")
        .accessibilityValue(helpText(snapshot))
        .popover(isPresented: $model.showingUsage, arrowEdge: .trailing) {
            UsageDetail(snapshot: snapshot)
        }
    }

    private func helpText(_ snapshot: UsageSnapshot) -> String {
        guard model.usage != nil else { return "Usage today" }
        let spent = "\(UsageSnapshot.money(snapshot.cost)), \(UsageSnapshot.count(snapshot.tokens)) tokens today"
        return snapshot.limitDescription.map { "\(spent) \($0)" } ?? spent
    }
}

/// The popover: today in detail, the recent trend, where it went, and the daily limit.
private struct UsageDetail: View {
    @Environment(AppModel.self) private var model
    let snapshot: UsageSnapshot
    @AppStorage(Preferences.dailyLimitKind) private var limitKind = "none"
    @AppStorage(Preferences.dailyLimitCost) private var limitCost = 0.0
    @AppStorage(Preferences.dailyLimitTokens) private var limitTokens = 0.0

    private struct Bar: Identifiable {
        let id: String
        let date: Date
        let value: Double
        let isToday: Bool
    }

    /// The last two weeks, with idle days as empty bars.
    private var bars: [Bar] {
        guard let summary = model.usage else { return [] }
        let byDay = Dictionary(summary.days.map { ($0.day, $0) }, uniquingKeysWith: { $1 })
        let showCost = summary.days.contains { $0.costUsd > 0 }
        let calendar = Calendar.current
        let todayKey = snapshot.today?.day ?? DayKey.string(for: Date(), in: .current)
        guard let end = DayKey.date(for: todayKey) else { return [] }
        return (0..<14).reversed().compactMap { back in
            guard let date = calendar.date(byAdding: .day, value: -back, to: end) else { return nil }
            let key = DayKey.string(for: date, in: .current)
            let day = byDay[key]
            let value = showCost ? (day?.costUsd ?? 0) : Double((day?.inputTokens ?? 0) + (day?.outputTokens ?? 0))
            return Bar(id: key, date: date, value: value, isToday: back == 0)
        }
    }

    private var chartShowsCost: Bool { model.usage?.days.contains { $0.costUsd > 0 } ?? false }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            header
            if let today = snapshot.today {
                Grid(alignment: .leading, horizontalSpacing: 18, verticalSpacing: 5) {
                    GridRow {
                        stat("Input", UsageSnapshot.count(today.inputTokens))
                        stat("Output", UsageSnapshot.count(today.outputTokens))
                        stat("Cached", UsageSnapshot.count(today.cacheReadTokens))
                    }
                    GridRow {
                        stat("Reasoning", UsageSnapshot.count(today.reasoningTokens))
                        stat("Sessions", "\(today.sessions)")
                        stat("API calls", "\(today.apiCalls)")
                    }
                }
            }
            if !bars.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text(chartShowsCost ? "Cost, last 14 days" : "Tokens, last 14 days")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Chart(bars) { bar in
                        BarMark(x: .value("Day", bar.date, unit: .day), y: .value("Amount", bar.value))
                            .foregroundStyle(bar.isToday ? AnyShapeStyle(.tint) : AnyShapeStyle(.tint.opacity(0.35)))
                            .cornerRadius(2)
                    }
                    .chartXAxis {
                        AxisMarks(values: .stride(by: .day, count: 7)) { _ in
                            AxisValueLabel(format: .dateTime.day().month(.abbreviated))
                        }
                    }
                    .chartYAxis {
                        AxisMarks(position: .trailing, values: .automatic(desiredCount: 3)) { value in
                            AxisGridLine()
                            AxisValueLabel {
                                if let amount = value.as(Double.self) {
                                    Text(chartShowsCost ? UsageSnapshot.money(amount) : UsageSnapshot.count(UInt64(amount)))
                                }
                            }
                        }
                    }
                    .frame(height: 96)
                    .accessibilityLabel("Daily usage for the last 14 days")
                }
            }
            if let models = model.usage?.models.prefix(3), !models.isEmpty {
                VStack(alignment: .leading, spacing: 5) {
                    Text("Top models, last 30 days")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    ForEach(Array(models), id: \.model) { entry in
                        HStack {
                            Text(entry.model).lineLimit(1).truncationMode(.middle)
                            Spacer()
                            Text(UsageSnapshot.count(entry.inputTokens + entry.outputTokens))
                                .foregroundStyle(.secondary)
                            if entry.costUsd > 0 {
                                Text(UsageSnapshot.money(entry.costUsd))
                                    .frame(minWidth: 52, alignment: .trailing)
                            }
                        }
                        .font(.system(size: 12.5).monospacedDigit())
                    }
                }
            }
            if let plan = model.usage?.plan {
                VStack(alignment: .leading, spacing: 5) {
                    HStack {
                        Text(plan.planName).font(.system(size: 12.5, weight: .medium))
                        Spacer()
                        Text("\(plan.remaining) left").font(.system(size: 12.5)).foregroundStyle(.secondary)
                    }
                    ProgressView(value: min(plan.fractionUsed, 1))
                    Text([plan.spent.isEmpty ? nil : "\(plan.spent) of \(plan.total) used", plan.renews].compactMap { $0 }.joined(separator: " · "))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Divider()
            limitEditor
        }
        .padding(16)
        .frame(width: 320)
    }

    private var header: some View {
        HStack(spacing: 14) {
            UsageRing(snapshot: snapshot, lineWidth: 7)
                .frame(width: 58, height: 58)
                .overlay {
                    if let fraction = snapshot.fraction {
                        Text(fraction.formatted(.percent.precision(.fractionLength(0))))
                            .font(.system(size: 12, weight: .semibold).monospacedDigit())
                    }
                }
            VStack(alignment: .leading, spacing: 1) {
                Text("Today")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text(UsageSnapshot.money(snapshot.cost))
                    .font(.system(size: 24, weight: .semibold).monospacedDigit())
                Text("\(UsageSnapshot.count(snapshot.tokens)) tokens\(snapshot.limitDescription.map { " · \($0)" } ?? "")")
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
    }

    private func stat(_ label: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(value).font(.system(size: 13, weight: .medium).monospacedDigit())
            Text(label).font(.caption).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var limitEditor: some View {
        HStack(spacing: 8) {
            Text("Daily limit")
                .font(.system(size: 12.5))
            Spacer()
            Picker("Daily limit", selection: $limitKind) {
                Text("None").tag("none")
                Text("Dollars").tag("cost")
                Text("Tokens").tag("tokens")
            }
            .labelsHidden()
            .fixedSize()
            if limitKind == "cost" {
                TextField("USD", value: $limitCost, format: .number.precision(.fractionLength(0...2)))
                    .textFieldStyle(.roundedBorder)
                    .multilineTextAlignment(.trailing)
                    .frame(width: 64)
                    .accessibilityLabel("Daily limit in dollars")
            } else if limitKind == "tokens" {
                TextField("Tokens", value: $limitTokens, format: .number.grouping(.automatic))
                    .textFieldStyle(.roundedBorder)
                    .multilineTextAlignment(.trailing)
                    .frame(width: 88)
                    .accessibilityLabel("Daily limit in tokens")
            }
        }
        .controlSize(.small)
    }
}
