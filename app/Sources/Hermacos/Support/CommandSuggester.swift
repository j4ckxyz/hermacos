import Foundation
import HermesCore

/// One row of the slash-command menu.
struct CommandSuggestion: Identifiable, Equatable {
    /// What the row stands for: `/usage`, or `/reasoning high`.
    let title: String
    let detail: String
    /// The text accepting the row puts in the message field.
    var completion: String { title + " " }
    var id: String { title }
}

/// Decides what the menu offers for a draft, and whether a draft is a command at all.
enum CommandSuggester {
    /// Commands this app answers itself, offered even before the server's list has loaded.
    private static let local: [SlashCommand] = [
        SlashCommand(name: "/new", description: "Start a new chat", category: "App", aliases: ["/clear", "/reset"], subcommands: []),
        SlashCommand(name: "/help", description: "List available commands", category: "App", aliases: [], subcommands: []),
    ]

    /// The server's commands, plus the local ones it doesn't already list.
    private static func all(_ commands: [SlashCommand]) -> [SlashCommand] {
        commands + local.filter { mine in !commands.contains { $0.name == mine.name } }
    }

    private static func find(_ name: String, in commands: [SlashCommand]) -> SlashCommand? {
        all(commands).first { $0.name == name || $0.aliases.contains(name) }
    }

    /// True when `text` starts with a known command, so `/etc/hosts is wrong` stays a message.
    static func isCommand(_ text: String, commands: [SlashCommand]) -> Bool {
        guard text.hasPrefix("/"), let first = text.split(whereSeparator: \.isWhitespace).first else { return false }
        return find(first.lowercased(), in: commands) != nil
    }

    static func suggestions(for draft: String, commands: [SlashCommand]) -> [CommandSuggestion] {
        guard draft.hasPrefix("/"), !draft.contains(where: \.isNewline) else { return [] }
        let catalog = all(commands)
        let parts = draft.split(separator: " ", maxSplits: 1, omittingEmptySubsequences: false)
        let typed = parts[0].lowercased()

        if parts.count == 1 {
            // Still typing the command name. Best matches first: names that start with what was
            // typed, then other names for a command, then names and descriptions containing it.
            let needle = String(typed.dropFirst())
            guard !needle.isEmpty else { return catalog.map { CommandSuggestion(title: $0.name, detail: $0.description) } }
            var ranked: [(rank: Int, suggestion: CommandSuggestion)] = []
            for command in catalog {
                if command.name.dropFirst().hasPrefix(needle) {
                    ranked.append((0, CommandSuggestion(title: command.name, detail: command.description)))
                } else if let alias = command.aliases.first(where: { $0.dropFirst().hasPrefix(needle) }) {
                    ranked.append((1, CommandSuggestion(title: alias, detail: command.description)))
                } else if command.name.contains(needle) {
                    ranked.append((2, CommandSuggestion(title: command.name, detail: command.description)))
                } else if needle.count >= 3, command.description.lowercased().contains(needle) {
                    ranked.append((3, CommandSuggestion(title: command.name, detail: command.description)))
                }
            }
            // A stable sort keeps the server's order within each rank.
            return ranked.enumerated()
                .sorted { ($0.element.rank, $0.offset) < ($1.element.rank, $1.offset) }
                .map(\.element.suggestion)
        }

        // Past the name: offer the command's fixed arguments while the first one is typed.
        let argument = parts[1].lowercased()
        guard !argument.contains(" "), let command = find(typed, in: catalog), !command.subcommands.isEmpty,
              !command.subcommands.contains(argument)
        else { return [] }
        return command.subcommands
            .filter { argument.isEmpty || $0.lowercased().hasPrefix(argument) }
            .map { CommandSuggestion(title: "\(command.name) \($0)", detail: command.description) }
    }

    /// `/help`: every command, grouped the way the server groups them.
    static func helpText(_ commands: [SlashCommand]) -> String {
        let catalog = all(commands)
        let width = (catalog.map(\.name.count).max() ?? 8) + 2
        var categories: [String] = []
        for command in catalog where !categories.contains(command.category) { categories.append(command.category) }
        return categories.map { category in
            let rows = catalog.filter { $0.category == category }.map { command in
                "  " + command.name.padding(toLength: width, withPad: " ", startingAt: 0) + command.description
            }
            return ([category.isEmpty ? "Commands" : category] + rows).joined(separator: "\n")
        }
        .joined(separator: "\n\n")
    }
}
