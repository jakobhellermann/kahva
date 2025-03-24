# kahva

A work in progress GUI for [JJ](https://github.com/jj-vcs/jj).

**Design goals:**

- simple UI
- interactive version of the CLI log graph
- drag and drop for intuitive operations

![demo image](./docs/demo.png)

## Configuration

kahva is configured using the regular jj user and repo configuration.

```toml
[revsets]
# kahva respects your usual log revset
log = "present(@) | ancestors(immutable_heads().., 4) | present(trunk())"
# but you can override it if you want to show a different set of commits
kahva-log = "::"
```