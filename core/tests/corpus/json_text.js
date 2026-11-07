// JSON text: strings borrowed from the input unless escaped, keys and
// values quoted in place from one-byte and two-byte strings, int32s
// written as digits.
var list = [];
for (var i = -3; i < 4; i++) list.push({ id: 'I' + i, qty: i * 1000, name: 'Item ' + i });
var text = JSON.stringify(list);
print(text);
print(JSON.stringify(JSON.parse(text)) === text, JSON.parse(text)[6].name);
print(JSON.stringify([2147483647, -2147483648, 0, -0, 1.5, 1e21, NaN, -Infinity]));
print(JSON.stringify({ 'é': 'ü', 'ж': 'Ω', 'a"b': 'c\\d', '\n': '\u0001\u001f', 's': '𐀀\ud800x\udfff' }));
var parsed = JSON.parse('{"plain":"text","esc":"a\\"b\\\\c\\/d\\n\\t\\u00e9\\u0416","\\u0041key":1,"emptyy":"","w":"жж"}');
print(parsed.plain, JSON.stringify(parsed.esc), parsed.Akey, parsed.emptyy === '', parsed.w.length, Object.keys(parsed).join());
print(JSON.parse('"\\ud83d\\ude00"').length, JSON.parse('["a",["b",{"c":"d"}]]')[1][1].c, JSON.parse(' "x" '));
print(JSON.parse('{"a":1,"a":2}').a, JSON.stringify(JSON.parse('{"__proto__":[]}')));
try { JSON.parse('"abc'); } catch (e) { print(e.name, e.message); }
try { JSON.parse('"a\\x"'); } catch (e) { print(e.name, e.message); }
try { JSON.parse('"a\u0001"'); } catch (e) { print(e.name, e.message); }
print(JSON.stringify({ a: [1, { b: 'c' }] }, null, 2));
print(JSON.stringify('é '), JSON.stringify(new String('s')), JSON.stringify({ toJSON() { return 'j'; } }));
var revived = JSON.parse('{"a":"x","b":["y","z\\n"]}', function (k, v) { return typeof v === 'string' ? v + '!' : v; });
print(JSON.stringify(revived));
