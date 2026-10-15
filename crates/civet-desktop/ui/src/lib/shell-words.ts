// Minimal POSIX-ish word splitting for the "custom command" agent option:
// whitespace separates words; single quotes are literal; double quotes allow
// \" and \\ escapes; a backslash outside quotes escapes the next character.

export function splitShellWords(input: string): string[] {
  const words: string[] = []
  let cur = ''
  let inWord = false
  let i = 0
  while (i < input.length) {
    const ch = input[i]!
    if (ch === "'") {
      const end = input.indexOf("'", i + 1)
      if (end < 0) throw new Error('unterminated single quote')
      cur += input.slice(i + 1, end)
      inWord = true
      i = end + 1
    } else if (ch === '"') {
      i++
      let closed = false
      while (i < input.length) {
        const c = input[i]!
        if (c === '"') {
          closed = true
          i++
          break
        }
        if (c === '\\' && (input[i + 1] === '"' || input[i + 1] === '\\')) {
          cur += input[i + 1]
          i += 2
          continue
        }
        cur += c
        i++
      }
      if (!closed) throw new Error('unterminated double quote')
      inWord = true
    } else if (ch === '\\' && i + 1 < input.length) {
      cur += input[i + 1]
      inWord = true
      i += 2
    } else if (/\s/.test(ch)) {
      if (inWord) words.push(cur)
      cur = ''
      inWord = false
      i++
    } else {
      cur += ch
      inWord = true
      i++
    }
  }
  if (inWord) words.push(cur)
  return words
}
