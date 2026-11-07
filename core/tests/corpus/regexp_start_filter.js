// A search skips the units its first instruction rejects; every match,
// index and lastIndex is the same as an attempt at each position.
const $esc = (v) => String(v).replace(/[&<>"']/g, (c) => c === "&" ? "&amp;" : c === "<" ? "&lt;" : c === ">" ? "&gt;" : c === '"' ? "&quot;" : "&#39;");
print($esc('plain text'), $esc('<a href="x">\'&\'</a>'), $esc(''), $esc('&'), $esc(12));
function show(s) {
  var out = '';
  for (var i = 0; i < s.length; i++) {
    var c = s.charCodeAt(i);
    out += c > 0x7e ? '\\u' + c.toString(16) : s[i];
  }
  return out;
}
function all(re, s) {
  var out = [], m;
  re.lastIndex = 0;
  while ((m = re.exec(s)) !== null) {
    out.push(m.index + ':' + show(m[0]) + (m.length > 1 ? '(' + m.slice(1).join(',') + ')' : '') + '@' + re.lastIndex);
    if (m[0] === '') re.lastIndex++;
  }
  return out.join(' ') || 'none';
}
print(all(/b/g, 'abcabcb'), all(/[x-z]/g, 'axbyczd'), all(/q/g, 'abc'), all(/(a)(b)?/g, 'xxabxa'));
print(all(/a|b/g, 'cacb'), all(/x*/g, 'axx'), all(/^b/gm, 'a\nb\nb'), all(/\bfoo/g, 'afoo foo'));
print(all(/o/gi, 'fOo'), all(/[^a]/g, 'aab'), all(/(?=c)/g, 'abc'), all(/(?<=a)b/g, 'abab'));
print(all(/[α-ω]/g, 'aβcδ'), all(/[a-cĀ-Ȁ]/g, 'xĀyb'), all(/é/g, 'café é'));
print(all(/[a-z]/gu, '\u{1F600}a\u{1F600}b'), all(/\u{1F600}/gu, 'x\u{1F600}y'), all(/[\uD83D]/g, '\u{1F600}'));
print(all(/./gu, '\u{1F600}'), all(/[\uDE00]/gu, '\u{1F600}\uDE00'));
var sticky = /b/y; sticky.lastIndex = 1;
print(sticky.test('abb'), sticky.lastIndex, sticky.test('abb'), sticky.lastIndex, sticky.test('abb'), sticky.lastIndex);
print('a-b-c'.split(/-/), 'abc'.search(/c/), 'abc'.search(/z/), 'aXbX'.replace(/X/g, '$&$&'), 'x'.match(/y/g));
print('one two'.replace(/[ ]/, '_'), 'aaa'.lastIndexOf('a'), /[0-9]/.exec('ab12').index, /z/.exec('ab'));
var long = 'lorem ipsum '.repeat(200) + '<end>';
print($esc(long).length, $esc(long).slice(-12));
var wide = 'ж'.repeat(50) + '&' + 'ж'.repeat(50);
print($esc(wide).length, $esc(wide).indexOf('&amp;'));
