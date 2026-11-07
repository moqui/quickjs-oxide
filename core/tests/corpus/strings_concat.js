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
