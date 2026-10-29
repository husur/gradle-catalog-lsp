# Security policy

Please report a vulnerability privately, through
[Report a vulnerability](https://github.com/husur/gradle-catalog-lsp/security/advisories/new),
rather than in a public issue. The language server sends the credentials of private repositories:
anything that could leak them, or send them to another host, is in scope.

Only the latest release is supported. The extension downloads the newest language server when Zed
starts, so a fix reaches every user without an update of the extension (a `gradle-catalog-lsp` found
in your `PATH` takes precedence, and has to be updated by hand).
