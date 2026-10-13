use cases::*;

macro_rules! case {
    ($($e:expr),* $(,)?) => { $( println!("{} = {:?}", stringify!($e), $e); )* };
}

fn main() {
    use aggregates::*;
    use enums::*;
    for (a, b) in [(1u32, 2u32), (1000, 7), (77, 0), (4_000, 123_456)] {
        case!(scalars::arith(a, b), scalars::checked(a, b), scalars::checked(u32::MAX - a, b));
        case!(scalars::signed(a as i64, -(b as i64)), scalars::bits(a as u16, b as u8));
        case!(scalars::casts(-(a as i8), b as u64 * 1_000_003), scalars::wrapping(a as u64, !(b as u64)));
        case!(scalars::wide((a as u128) << 70, b as u64), scalars::compare(a as i32, b as i32));
        case!(scalars::counting((a as u64) << 9 | b as u64));
    }
    case!(control::collatz(27), control::nested(40), control::fib(15));
    case!(control::classify(-5), control::classify(0), control::classify(7), control::classify(99));

    case!(small(Small(7, 300)), pair(Pair { a: 5, b: 1 << 40 }, 9), odd(Odd(1, 2, 3)));
    let big_value = Big { a: 11, b: [1, 2, 3, 4, 5], c: -3 };
    case!(big(&big_value, 50), big_by_value(big_value), tuple((250, 9, 1 << 50)), array([1, 2, 3, 4]));
    case!(nested_field(&(Pair { a: 1, b: 2 }, Small(3, 4))));
    let mut p = Pair { a: 1, b: 21 };
    mutate(&mut p);
    case!(p);

    for n in 0..4 {
        case!(plain(n), plain_back(plain(n)), option(n), niche_bool(n as u8), niche_bool_back(niche_bool(n as u8)));
        case!(data(n * 5), data_back(data(n * 5)), result(n as i32 - 2), ordering(n, 2));
    }
    case!(explicit(Explicit::Low), explicit(Explicit::Mid), explicit(Explicit::High));
    case!(option_ref(&Some(4)), option_ref(&None), niche_ref(true, &5), niche_ref(false, &5));
    for c in *b"a7!?Z" {
        case!(nested_niche(c, 3), nested_niche_back(&nested_niche(c, 3)));
    }

    let xs = [3u32, 1, 4, 1, 5, 9, 2, 6];
    case!(slices::sum(&xs), slices::sum(&[]), slices::sum_iter(&xs), slices::tail(&xs), slices::window(&xs, 2, 5));
    case!(slices::first_last(&[]), slices::first_last(&[4]), slices::first_last(&[4, 5, 6]));
    let mut rev = [1u16, 2, 3, 4, 5];
    slices::reverse(&mut rev);
    case!(rev, slices::str_len("héllo"), slices::bytes_hash("rustd"), slices::literal(0), slices::literal(1));
    case!(slices::array_slice(&[1, 2, 3, 4, 5, 6]), slices::get(&[7, 8], 1), slices::get(&[7, 8], 2));
    case!(slices::is_ascii_digit_run(b"123ab"));

    let bytes = *b"xyz";
    let mut cursor = pointers::Cursor { start: bytes.as_ptr(), end: unsafe { bytes.as_ptr().add(3) } };
    case!(pointers::peek(&cursor), pointers::advance(&mut cursor, 2), pointers::peek(&cursor));
    case!(pointers::advance(&mut cursor, 1), pointers::peek(&cursor));
    let words = [1u32, 2];
    case!(pointers::compare(&words[0], &words[1]), pointers::compare(&words[1], &words[1]));
    let mut slot = 5u64;
    case!(pointers::write(&mut slot, 6), slot);

    case!(consts::limit(50), consts::limit(5), consts::table(6), consts::name(1), consts::config(3));
    case!(consts::local_array(4), consts::repeat(9));

    case!(generics::picks(5, 6), generics::swaps(1, 2), generics::inline(5), generics::methods(10));
    case!(generics::statics(3, 4), generics::option_map(Some(4)), generics::option_map(None));
    case!(generics::core_generics(3, 9));

    case!(closures::capture(10, &xs), closures::capture_mut(5), closures::iter_chain(&xs));
    case!(closures::fn_pointer(|x| x * 3, 2));

    case!(callables::reify(4), callables::reify_pair(7), callables::generic_closure(3, 5));
    case!(callables::generic_mut(6), callables::generic_once(9), callables::takes_fn(|a, b| a ^ b, 9));

    let b3 = odd::rot3(odd::B3(1, 2, 3));
    case!((b3.0, b3.1, b3.2), odd::rot5(odd::B5([1, 2, 3, 4, 5])).0, odd::chain(odd::B3(7, 8, 9)));
    let b6 = odd::rot6(odd::B6(100, 200, 300));
    case!((b6.0, b6.1, b6.2), odd::rot7(odd::B7([1, 2, 3, 4, 5, 6, 7]), 99).0);

    case!(dynamic::call(&dynamic::Dog(4)), dynamic::call(&dynamic::Cat), dynamic::make(true), dynamic::make(false));

    case!(heap::boxed(1), heap::vec(5), heap::string(3), heap::consume(vec![1, 2, 3]), heap::format(4, "x"));
}
