// A string and an int32 join in one step; String.prototype.concat (which
// template literals call) joins its converted arguments once.
var ints = [0, 7, -7, 10, 2147483647, -2147483648, 1000000];
var out = [];
for (var i = 0; i < ints.length; i++) out.push('x' + ints[i], ints[i] + 'y', 'αβ' + ints[i], ints[i] + 'γ');
print(out.join('|'));
var long = 'abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz';
print((long + -2147483648).length, (-5 + long).slice(0, 4), (long + 12).slice(-4));
print('' + 0, 5 + '', '' + -0, 1.5 + 'z', 'z' + 1.5);
var built = '';
for (var i = 0; i < 200; i++) built += 'k' + i;
print(built.length, built.slice(0, 12), built.slice(-8));
var calls = [];
var o = { toString: function () { calls.push('o'); return 'O'; } };
var p = { toString: function () { calls.push('p'); return 'P\u{3b1}'; } };
print('a'.concat(1, o, -2, p, null, undefined, true, 1.25), calls.join(''));
print('a'.concat(), ''.concat(''), ''.concat('', ''), 'b'.concat(''), ''.concat(5), ''.concat('é'));
var many = [];
for (var i = 0; i < 40; i++) many.push(i % 3 ? i : 'v' + i);
print(''.concat.apply('s', many));
many[30] = 'ω';
print(''.concat.apply('s', many).length, ''.concat.apply('s', many).slice(-12));
print(String.prototype.concat.call(42, 1, 'a'), String.prototype.concat.call(true, '!'));
try { String.prototype.concat.call(null, 1); } catch (e) { print(e.name); }
try { 'a'.concat(Symbol('s')); } catch (e) { print(e.name); }
var item = { id: 'I7', qty: 4, name: 'Item <7>' };
print(`<li id="${item.id}" class="${item.qty > 3 ? 'big' : 'small'}">${item.name}: ${item.qty}</li>`);
print(`${-1}${0}${'é'}${[1, 2]}${{}}`, `${'ü'}${'\u{1F600}'}`.length);
// A rope receiver, more than 16 arguments with two-byte strings, a result
// long enough to join through ropes, and the string length limit.
var rope = 'a'.repeat(40) + 'b'.repeat(40);
print(rope.concat(1, -2, 'c'), rope.concat(7).length);
var args = [];
for (var i = 0; i < 24; i++) args.push(i % 4 === 0 ? 'ж' + i : i % 4 === 1 ? i : 'p' + i);
var joined = String.prototype.concat.apply('ü', args);
print(joined, joined.length);
var big = 'y'.repeat(1000).concat(12345, 'ж', 'z'.repeat(30), -7);
print(big.length, big.slice(995, 1012), big.slice(-5));
var d = 'x'.repeat(2 ** 20), parts = [];
for (var k = 20; k < 30; k++) { parts.unshift(d); if (k < 29) d = d + d; }
var near = parts.reduce(function (a, b) { return a + b; }) + 'x'.repeat(2 ** 20 - 5);
print(near.length === 2 ** 30 - 5);
try { near.concat(123456); } catch (e) { print(e.name, e.message); }
print(near.concat('ab', 'cd').length === 2 ** 30 - 1, near.concat(1234).length === 2 ** 30 - 1);
try { `${near}${123456}`; } catch (e) { print(e.name, e.message); }
