use regex_automata::meta::Regex;

fn main() {
    for pattern in [r"a+b*", r"(?i)foo|bar\d{2,5}", r"[a-z&&[^aeiou]]+\b", r"\p{Greek}+", r"(unclosed"] {
        match regex_syntax::Parser::new().parse(pattern) {
            Ok(hir) => println!("{pattern} => {hir} / {:?}", hir.properties().minimum_len()),
            Err(err) => println!("{pattern} => error: {err}"),
        }
    }
    let re = Regex::new(r"(?P<y>\d{4})-(?P<m>\d{2})|qu[a-z]+k").unwrap();
    let hay = "on 2026-10 the quick brown fox, 1999-12";
    for m in re.find_iter(hay) {
        println!("{:?} {}", m.range(), &hay[m.range()]);
    }
}
