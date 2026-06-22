//! Micro-benchmark for `extract_main_content_v4` on inline HTML fixtures.
//! Measures both v3 (the previous default) and v4-with-Firecrawl (the new
//! path) so we can compare quality and latency per page type.
//!
//! Run with: `cargo test -p crw-extract --features firecrawl-extractor --release
//!            -- --ignored --nocapture micro_bench_v3_vs_v4`
//!
//! The fixtures are intentionally varied: one long-form article
//! (Wikipedia-style), one documentation page (rust-lang.org), one product
//! page (Amazon-style), one forum thread. The v4 router should delegate
//! Article+Doc to Firecrawl and keep Product+Forum on v3.
//!
//! Note: HTML fixtures use `r##"..."##` (double-#) raw strings so the
//! inner `href="#anchor"` attribute quotes don't terminate the string
//! early.

use crw_extract::{extract_main_content_v3, extract_main_content_v4};

const ARTICLE_HTML: &str = r##"
<!DOCTYPE html>
<html>
<head>
    <title>Rust (programming language) - Wikipedia</title>
    <meta name="description" content="Rust is a general-purpose programming language emphasizing performance, type safety, and concurrency.">
    <script type="application/ld+json">{"@context":"https://schema.org","@type":"Article","headline":"Rust"}</script>
</head>
<body>
    <nav><ul><li><a href="/wiki/Main_Page">Main page</a></li><li><a href="/wiki/Contents">Contents</a></li></ul></nav>
    <div id="mw-panel"><div class="portlet"><h3>Navigation</h3><ul><li>foo</li><li>bar</li></ul></div></div>
    <main id="content">
        <h1>Rust (programming language)</h1>
        <p>Rust is a general-purpose programming language that emphasizes performance, type safety, and concurrency. It enforces memory safety — that is, that all references point to valid memory — without requiring the use of a garbage collector or reference counting present in other memory-safe languages.</p>
        <p>To simultaneously enforce memory safety and prevent concurrent data races, Rust's borrow checker tracks the object lifetime and ownership of all references in a program during compilation. Rust is popular for systems programming but also has applications in web development, game development, and embedded systems.</p>
        <h2>History</h2>
        <p>Rust began as a personal project by Mozilla employee Graydon Hoare in 2006. Hoare began the project to address shortcomings in C++ that he had been working with while working at Mozilla. The language grew out of a need for a memory-safe, concurrent, and practical systems language.</p>
        <h2>Syntax</h2>
        <p>Rust's syntax is similar to that of C and C++, with blocks of code delimited by curly braces and control flow keywords such as if, else, while, and for. The language is designed to be memory-safe, and it does not allow null pointers, dangling pointers, or data races in safe code.</p>
        <p>Memory is managed through the concept of ownership, which means that values are owned by variables, and values can be moved or borrowed. The borrow checker enforces these rules at compile time, ensuring that references do not outlive the data they point to.</p>
        <h2>Memory safety</h2>
        <p>Rust is designed to be memory safe. It does not permit null pointers, dangling pointers, or data races in safe code. References and borrows are checked at compile time to ensure that they are always valid.</p>
        <p>Unsafe code, which bypasses some of these checks, is allowed in marked blocks, but the developer is responsible for ensuring correctness in those sections.</p>
    </main>
    <footer>Copyright Wikipedia. This page is available under the Creative Commons Attribution-ShareAlike License.</footer>
    <aside class="sidebar"><p>Related articles</p><ul><li>C++</li><li>Go</li></ul></aside>
</body>
</html>
"##;

const DOC_HTML: &str = r##"
<!DOCTYPE html>
<html>
<head><title>std::vec::Vec - Rust Documentation</title></head>
<body>
    <nav><a href="#">std</a> &gt; <a href="#">vec</a> &gt; Vec</nav>
    <main>
        <h1>std::vec::Vec</h1>
        <pre><code>pub struct Vec&lt;T, A: Allocator = Global&gt; { /* fields omitted */ }</code></pre>
        <p>A contiguous growable array type, written as <code>Vec&lt;T&gt;</code>, short for 'vector'.</p>
        <h2>Methods</h2>
        <ul>
            <li><a href="#method.new">new</a> - Constructs a new, empty Vec.</li>
            <li><a href="#method.push">push</a> - Appends an element to the back of a collection.</li>
            <li><a href="#method.pop">pop</a> - Removes the last element from a vector and returns it.</li>
        </ul>
        <h2>Examples</h2>
        <pre><code>let mut v = Vec::new();
v.push(1);
v.push(2);
assert_eq!(v, vec![1, 2]);</code></pre>
    </main>
    <footer>Copyright The Rust Project Developers.</footer>
</body>
</html>
"##;

const PRODUCT_HTML: &str = r##"
<!DOCTYPE html>
<html>
<head><title>Acme Widget - Amazon.com</title></head>
<body>
    <nav>Breadcrumb: Home / Electronics / Widgets</nav>
    <main>
        <h1>Acme Widget</h1>
        <div class="price">$19.99</div>
        <div class="description">
            <p>The Acme Widget is a high-quality widget that does everything you need. Buy it now and save 10% with code WIDGET10.</p>
        </div>
        <div class="specs">
            <h2>Specifications</h2>
            <table>
                <tr><th>Brand</th><td>Acme</td></tr>
                <tr><th>Model</th><td>W-2000</td></tr>
                <tr><th>Weight</th><td>2.5 lbs</td></tr>
            </table>
        </div>
        <div class="reviews">
            <h2>Customer reviews</h2>
            <p>4.5 out of 5 stars — 1,234 reviews</p>
        </div>
    </main>
    <footer>Copyright 1996-2026, Amazon.com, Inc.</footer>
</body>
</html>
"##;

const FORUM_HTML: &str = r##"
<!DOCTYPE html>
<html>
<head><title>Why use Rust over C++? - Stack Overflow</title></head>
<body>
    <nav>Stack Overflow / Questions / Tags / Users</nav>
    <main>
        <h1>Why use Rust over C++?</h1>
        <div class="question">
            <p>I'm starting a new systems project and trying to decide between Rust and modern C++. What are the main trade-offs?</p>
            <div class="tags">rust, c++, systems-programming</div>
        </div>
        <h2>3 Answers</h2>
        <div class="answer">
            <p>The main reason is memory safety without garbage collection. Rust's borrow checker prevents data races and use-after-free at compile time.</p>
            <div class="votes">42</div>
        </div>
        <div class="answer">
            <p>Modern C++ has smart pointers and move semantics that go a long way, but you can still shoot yourself in the foot. Rust's safety is enforced, not aspirational.</p>
            <div class="votes">17</div>
        </div>
    </main>
    <footer>Site design / logo copyright 2026 Stack Exchange Inc.</footer>
</body>
</html>
"##;

#[test]
#[ignore = "long-running; run manually with --ignored --nocapture"]
fn micro_bench_v3_vs_v4() {
    use std::time::Instant;

    let fixtures: &[(&str, &str)] = &[
        ("article", ARTICLE_HTML),
        ("doc", DOC_HTML),
        ("product", PRODUCT_HTML),
        ("forum", FORUM_HTML),
    ];

    println!("\n{:<10} {:<10} {:<10} {:<10} {:<10}", "fixture", "extractor", "ms", "bytes", "quality");
    println!("{}", "-".repeat(60));

    for (name, html) in fixtures {
        // Warm up
        let _ = extract_main_content_v3(html, None);
        let _ = extract_main_content_v4(html, None, "https://example.com");

        let iterations = 100;
        let url = "https://example.com";

        // v3
        let start = Instant::now();
        for _ in 0..iterations {
            let _ = extract_main_content_v3(html, None);
        }
        let v3_ms = start.elapsed().as_millis() as f64 / iterations as f64;
        let v3_result = extract_main_content_v3(html, None);
        println!(
            "{:<10} {:<10} {:<10.3} {:<10} {:<10.2}",
            name, "v3", v3_ms, v3_result.result.markdown.len(), v3_result.result.quality
        );

        // v4
        let start = Instant::now();
        for _ in 0..iterations {
            let _ = extract_main_content_v4(html, None, url);
        }
        let v4_ms = start.elapsed().as_millis() as f64 / iterations as f64;
        let v4_result = extract_main_content_v4(html, None, url);
        println!(
            "{:<10} {:<10} {:<10.3} {:<10} {:<10.2}",
            name, "v4", v4_ms, v4_result.result.markdown.len(), v4_result.result.quality
        );
    }
}
