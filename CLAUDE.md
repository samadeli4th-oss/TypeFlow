# TypeFlow project rules

## Never monitor keystrokes in password or secure fields

This rule has no exceptions and takes priority over any feature, including hints, auto-fix,
auto-correct, learning, undo, statistics and debugging.

**Secure fields** are password, PIN, one-time code, card number and security code fields, any control
marked as protected, and credential prompts in terminals or other apps. A field whose type can't be
confirmed counts as secure.

1. **Check first.** Confirm a field is safe before translating or storing a key. A key from an
   unconfirmed field is passed on and forgotten. Recording it and erasing it later is not allowed.
2. **Fail closed.** An unknown, unanswered, failed or timed-out field check means the field is secure.
3. **Keep nothing.** Keys from secure fields must never reach `ToUnicodeEx`, the typing buffer, word
   candidates, counters, logs, debug output, files, the clipboard or crash reports.
4. **Derive nothing.** Do not keep their length, timing, key counts, hashes or statistics.
5. **Read only the field's type.** Field checks may read a control's role, state and input type,
   never its name, value or contents.
6. **Handle hooks strictly.** Windows delivers every key to the low-level hook. For a secure field,
   the hook passes the key to the next hook and does nothing else with it.
7. **Use one gate.** Every feature that reads typed text goes through the same secure-field check, and
   adds a test proving that secure-field input can't reach it.
8. **Keep claims true.** The README's privacy section must describe what the code actually does.
