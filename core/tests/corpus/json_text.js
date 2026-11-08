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
var rows = []; for (var i = 0; i < 4; i++) rows.push({ id: i, name: 'n' + i, 'q"k\n': i / 3, wide: 'β' + i });
print(JSON.stringify(rows));
var hidden = { a: 1, b: 2 }; Object.defineProperty(hidden, 'c', { value: 3, enumerable: false }); Object.defineProperty(hidden, 'd', { get() { return 4; }, enumerable: true });
print(JSON.stringify([hidden, { a: 1, b: 2 }]));
print(JSON.stringify({ b: 1, 2: 'two', a: 3, 1: 'one', [Symbol('s')]: 5 }));
var inherited = Object.create({ toJSON() { return 'inherited'; } }); inherited.x = 1;
print(JSON.stringify([{ x: 1 }, inherited, { x: 1, toJSON() { return 'own'; } }]));
class Point { constructor(x, y) { this.x = x; this.y = y; } }
print(JSON.stringify([new Point(1, 2), new Point(3, 4)]));
var moved = { x: 1 }; Object.setPrototypeOf(moved, { toJSON() { return 'moved'; } });
print(JSON.stringify([{ x: 1 }, moved]));
var victim = { a: 1, b: 2, c: 3 }, other = { a: 1, b: 2, c: 3 };
var deleter = { toJSON() { delete victim.b; victim.c = 'changed'; victim.z = 9; return 'd'; } };
print(JSON.stringify([other, { first: deleter, then: victim }]));
victim = { a: 1, b: 2, c: 3 };
print(JSON.stringify({ p: { toJSON() { victim.b = 'x'; return 1; } }, v: victim }));
print(JSON.stringify([1, , 3, undefined, function () {}, Symbol('x'), NaN, -0, 1e21, 0.1]));
Array.prototype[1] = 'proto'; print(JSON.stringify([1, , 3])); delete Array.prototype[1];
print(JSON.stringify(new Proxy([1, 2], {})), JSON.stringify(new Proxy({ a: 1 }, {})));
print(JSON.stringify({ s: '\ud800 \udc00 😀 \u0001\u001f "\\ é' }));
print(JSON.stringify([{ a: [1, { b: 2 }], c: {} }, []], null, '\t'));
print(JSON.stringify({ a: 1, b: [2] }, null, 2));
print(JSON.stringify({ a: 1, b: { c: 2 } }, ['b', 'c']));
print(JSON.stringify({ a: 1, b: 'x' }, function (k, v) { return typeof v === 'number' ? v * 10 : v; }));
Object.prototype.toJSON = function () { return 'everywhere'; }; print(JSON.stringify([{ a: 1 }, [2]])); delete Object.prototype.toJSON;
var dict = {}; for (var j = 0; j < 80; j++) dict['k' + j] = j; delete dict.k3; print(JSON.stringify(dict).length);
print(JSON.stringify({ x: 'β' }), JSON.stringify({ 'β': 1, a: 'b' }), JSON.stringify(['a', 'Ā'.slice(0, 0)]));
var o = { a: { toJSON() { delete o.b; o.c = 'changed'; o.d = 4; return 1; } }, b: 2, c: 3 };
var o2 = { a: { toJSON() { o2.b = 'x'; return 1; } }, b: 2 };
print(JSON.stringify(o), JSON.stringify(o2));
print(JSON.stringify([{ toJSON() { Object.prototype.toJSON = function () { return 'late'; }; return 0; } }, { a: 1 }])); delete Object.prototype.toJSON;
var p = JSON.parse('{"a":1,"b":2,"a":3,"__proto__":{"x":1},"7":"seven","4294967295":"big","c":-0,"d":[123456789012345,1234567890123456,12345678901234567890,-42,0,1e2,1.5,-0.0]}');
print(JSON.stringify(p), Object.keys(p).join(','), 1 / p.c, 1 / p.d[7], Object.getPrototypeOf(p) === Object.prototype, p.__proto__.x);
var many = '{' + Array.from({ length: 70 }, function (_, i) { return '"k' + i + '":' + i; }).join(',') + ',"k5":"again"}';
var m = JSON.parse(many); print(Object.keys(m).length, m.k5, m.k69, Number('  12  '), Number('1e400'), Number('0x1F'), Number('9007199254740993'));
