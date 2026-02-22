use std::time::Duration;
use flate2::read::{GzDecoder, DeflateDecoder};
use std::io::Read;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let feeds = vec![
        ("1Link.Fun 科技杂谈", "https://techhub.social/users/1link.rss"),
        ("AI Focus", "https://aifoc.us/index.xml"),
        ("AI洞察日报", "https://justlovemaki.github.io/CloudFlare-AI-Insight-Daily/rss.xml"),
        ("Amp News", "https://ampcode.com/news.rss"),
        ("Andy Stewart", "https://manateelazycat.github.io/feed.xml"),
        ("Anil Dash", "https://www.anildash.com/feed.xml"),
        ("Annie Vella", "https://annievella.com/index.xml"),
        ("Anthropic Engineering", "https://rsshub.umzzz.com/anthropic/engineering"),
        ("Armin Ronacher", "https://lucumr.pocoo.org/feed.atom"),
        ("Arpit Bhayani", "https://arpitbhayani.me/rss.xml"),
        ("Articles - Kerrick Long", "https://kerrick.blog/category/articles/feed/"),
        ("BestBlogs.dev", "https://www.bestblogs.dev/feeds/rss"),
        ("Blog | Phodal", "https://www.phodal.com/blog/feeds/rss/"),
        ("ByteByteGo Newsletter", "https://blog.bytebytego.com/feed"),
        ("CatCoding", "https://catcoding.me/atom.xml"),
        ("Cline Official Blog", "https://rsshub.umzzz.com/cline/blog"),
        ("Coding Horror", "https://blog.codinghorror.com/rss/"),
        ("Cognition", "https://api.xgo.ing/rss/user/4cc14cbd15c74e189d537c415369e1a7"),
        ("DBA Notes", "https://dbanotes.net/feed"),
        ("Daniel Lemire", "https://lemire.me/blog/feed/"),
        ("David Heinemeier Hansson", "https://world.hey.com/dhh/feed.atom"),
        ("Drew Breunig", "https://www.dbreunig.com/feed.xml"),
        ("Elmagnifico's Blog", "https://elmagnifico.tech/feed.xml"),
        ("Embrace The Red", "https://embracethered.com/blog/index.xml"),
        ("Frank DENIS", "https://00f.net/atom.xml"),
        ("Geoffrey Litt", "https://www.geoffreylitt.com/feed.xml"),
        ("Gino Notes", "https://www.ginonotes.com/feed.xml"),
        ("HackerNews每日摘要", "https://www.supertechfans.com/cn/index.xml"),
        ("HelloGitHub 月刊", "https://hellogithub.com/rss"),
        ("Hi, DIYgod", "https://diygod.cc/feed"),
        ("InfoQ 后端", "https://rsshub.umzzz.com/infoq/topic/1174"),
        ("InfoQ 架构", "https://rsshub.umzzz.com/infoq/topic/architecture"),
        ("Jason Fried", "https://world.hey.com/jason/feed.atom"),
        ("Just For Fun", "https://selfboot.cn/atom.xml"),
        ("Last Week in AI", "https://lastweekin.ai/feed"),
        ("Lenny's Newsletter", "https://www.lennysnewsletter.com/feed"),
        ("Lobsters", "https://lobste.rs/rss"),
        ("LocalThunk", "https://localthunk.com/?format=rss"),
        ("Maggie Appleton", "https://maggieappleton.com/rss.xml"),
        ("Martin Fowler", "https://martinfowler.com/feed.atom?code=ff255dc1705b8cfb08b9e88bf9514048"),
        ("Modern Web Dev", "https://paul.kinlan.me/index.xml"),
        ("NSHipster", "https://nshipster.com/feed.xml"),
        ("No Mercy / No Malice", "https://www.profgalloway.com/feed/"),
        ("OneV's Den", "https://onevcat.com/feed.xml"),
        ("Owen的博客", "https://www.owenyoung.com/atom.xml"),
        ("PH今日热榜", "https://decohack.com/category/producthunt/feed/"),
        ("ParadeDB", "https://www.paradedb.com/feed.xml"),
        ("Pluralistic", "https://pluralistic.net/feed/"),
        ("PromptArmor Blog", "https://promptarmor.substack.com/feed"),
        ("RSS Actualités CNIL", "https://www.cnil.fr/en/rss.xml"),
        ("Randy's Blog", "https://lutaonan.com/rss.xml"),
        ("Raph Koster", "https://www.raphkoster.com/feed/"),
        ("Red Blob Games", "https://www.redblobgames.com/blog/posts.xml"),
        ("Release notes Folo", "https://github.com/RSSNext/Folo/releases.atom"),
        ("Rust 编程实战学习", "https://rs.bifuba.com/rss.xml"),
        ("Rust语言中文社区", "https://rsshub.umzzz.com/rustcc/news"),
        ("ScarSu", "https://scarsu.com/rss.xml"),
        ("Schneier on Security", "https://www.schneier.com/feed/atom/"),
        ("Second Brain", "https://www.ssp.sh/brain/index.xml"),
        ("Shrivu's Substack", "https://blog.sshh.io/feed"),
        ("Simon Willison", "https://simonwillison.net/atom/everything/"),
        ("Sinclair Target", "https://sinclairtarget.com/index.xml"),
        ("Steve Klabnik", "https://steveklabnik.com/feed.xml"),
        ("Steve Yegge", "https://medium.com/feed/@steve-yegge"),
        ("Systems Thinking", "https://medium.com/feed/tag/systems-thinking"),
        ("TaurusXin", "https://www.taurusxin.com/index.xml"),
        ("Tech on JmsDnns", "https://jmsdnns.com/tech/index.xml"),
        ("The Cascade", "https://thecascade.dev/rss.xml"),
        ("Tw93 Blog", "https://tw93.fun/feed.xml"),
        ("Val Town Blog", "https://blog.val.town/rss.xml"),
        ("Xiaowen Zhang", "https://world.hey.com/xiaowen/feed.atom"),
        ("Zara's Newsletter", "https://zarazhang.substack.com/feed"),
        ("Zed Industries", "https://zed.dev/blog.rss"),
        ("allan.reyes.sh", "https://allan.reyes.sh/index.xml"),
        ("chadnauseam.com", "https://chadnauseam.com/rss.xml"),
        ("delphij's Chaos", "https://blog.delphij.net/atom.xml"),
        ("devblogs.microsoft.com", "https://devblogs.microsoft.com/oldnewthing/feed"),
        ("dynomight.net", "https://dynomight.net/feed.xml"),
        ("lcamtuf.substack.com", "https://lcamtuf.substack.com/feed"),
        ("maurycyz.com", "https://maurycyz.com/index.xml"),
        ("maxOS", "https://maxoxo.me/rss/"),
        ("mitchellh.com", "https://mitchellh.com/feed.xml"),
        ("shkspr.mobi", "https://shkspr.mobi/blog/feed/"),
        ("sunshowers", "https://sunshowers.io/index.xml"),
        ("the singularity is nearer", "https://geohot.github.io//blog/feed.xml"),
        ("utcc.utoronto.ca", "https://utcc.utoronto.ca/~cks/cspace-stub-feeds/cloud-source.atom"),
        ("xeiaso.net", "https://xeiaso.net/blog.rss"),
        ("Mario Zechner", "https://mariozechner.at/rss.xml"),
        ("云风的 BLOG", "https://blog.codingnow.com/atom.xml"),
        ("从不说安全词", "https://jt26wzz.com/rss.xml"),
        ("分享创造日报", "https://v2ex-create.nexmm.com/rss.xml"),
        ("太隐", "https://wangyurui.com/feed.xml"),
        ("奇客Solidot", "https://www.solidot.org/index.rss"),
        ("宝玉的分享", "https://s.baoyu.io/feed.xml"),
        ("小胡子哥", "https://www.barretlee.com/rss2.xml?code=989a6aaf710a7bfc999d95249d24a1ef"),
        ("小道求职消息日报", "https://www.edgeer.net/feed.rss"),
        ("开源服务指南", "https://osguider.com/blog/index.xml"),
        ("方糖07", "https://ft07.com/feed/"),
        ("月光博客", "https://www.williamlong.info/rss.xml"),
        ("木匣子", "https://blog.mutoo.im/atom.xml"),
        ("王福强的个人博客", "https://afoo.me/feeds.xml"),
        ("白宦成", "https://www.ixiqin.com/feed/"),
        ("美团技术团队", "https://tech.meituan.com/feed/"),
        ("菠萝油与天光墟", "https://ramsayleung.github.io/zh/index.xml"),
        ("让小产品独立变现", "https://www.ezindie.com/feed/rss.xml"),
        ("阮一峰的网络日志", "http://www.ruanyifeng.com/blog/atom.xml"),
        ("面向信仰编程", "https://draveness.me/feed.xml"),
        // RSSHub and wechat2rss feeds will be tested separately
    ];

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        .timeout(Duration::from_secs(30))
        .build()?;

    println!("=== Testing RSS Feeds ===\n");

    for (name, url) in feeds {
        print!("{} ... ", name);

        match test_feed(&client, url).await {
            Ok(result) => println!("✓ {}", result),
            Err(e) => println!("✗ {}", e),
        }
    }

    Ok(())
}

async fn test_feed(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = client
        .get(url)
        .header("Accept", "application/rss+xml, application/xml, text/xml, application/atom+xml")
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .header("Accept-Encoding", "gzip, deflate, br")
        .send()
        .await
        .map_err(|e| format!("Connection error: {}", e))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status));
    }

    let bytes = response.bytes().await.map_err(|e| format!("Failed to read body: {}", e))?;

    // Try to decompress if needed
    let content = try_decompress(&bytes)?;

    // Check content type
    if content.contains("<?xml") || content.contains("<rss") || content.contains("<feed") || content.contains("<entry>") {
        // Count items
        let item_count = content.matches("<item>").count()
            .max(content.matches("<entry>").count());
        Ok(format!("OK ({} items)", item_count))
    } else if content.contains("<!DOCTYPE") || content.contains("<html") {
        Err("HTML page (not a valid feed)".to_string())
    } else if content.trim().starts_with('{') {
        Err("JSON format (not supported)".to_string())
    } else if content.trim().is_empty() {
        Err("Empty response".to_string())
    } else {
        let preview = if content.len() > 100 {
            format!("{}...", &content[..100])
        } else {
            content.clone()
        };
        Err(format!("Unknown format. Preview: {}", preview))
    }
}

fn try_decompress(bytes: &[u8]) -> Result<String, String> {
    if bytes.is_empty() {
        return Err("Empty bytes".to_string());
    }

    eprintln!("  [DEBUG] Bytes: {} bytes, first 4: {:02x} {:02x} {:02x} {:02x}", bytes.len(), bytes[0], bytes.get(1).unwrap_or(&0), bytes.get(2).unwrap_or(&0), bytes.get(3).unwrap_or(&0));

    // Check for gzip
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        eprintln!("  [DEBUG] Detected gzip, attempting decompression...");
        let mut decoder = GzDecoder::new(bytes);
        let mut decompressed = Vec::new();
        match decoder.read_to_end(&mut decompressed) {
            Ok(_) => {
                eprintln!("  [DEBUG] Gzip decompressed to {} bytes", decompressed.len());
                match String::from_utf8(decompressed.clone()) {
                    Ok(text) => {
                        eprintln!("  [DEBUG] UTF-8 conversion successful");
                        return Ok(text);
                    }
                    Err(_) => {
                        eprintln!("  [DEBUG] UTF-8 conversion failed, trying Latin-1");
                        let text = decompressed.iter().map(|&b| b as char).collect::<String>();
                        return Ok(text);
                    }
                }
            }
            Err(e) => {
                eprintln!("  [DEBUG] Gzip decompression failed: {}", e);
                return Err(format!("Gzip error: {}", e));
            }
        }
    }

    // Check for deflate
    if bytes.len() >= 1 && bytes[0] == 0x78 {
        eprintln!("  [DEBUG] Detected deflate, attempting decompression...");
        let mut decoder = DeflateDecoder::new(bytes);
        let mut decompressed = Vec::new();
        match decoder.read_to_end(&mut decompressed) {
            Ok(_) => {
                eprintln!("  [DEBUG] Deflate decompressed to {} bytes", decompressed.len());
                match String::from_utf8(decompressed.clone()) {
                    Ok(text) => return Ok(text),
                    Err(_) => {
                        let text = decompressed.iter().map(|&b| b as char).collect::<String>();
                        return Ok(text);
                    }
                }
            }
            Err(e) => {
                eprintln!("  [DEBUG] Deflate decompression failed: {}", e);
                return Err(format!("Deflate error: {}", e));
            }
        }
    }

    // Try UTF-8
    match String::from_utf8(bytes.to_vec()) {
        Ok(text) => Ok(text),
        Err(_) => {
            eprintln!("  [DEBUG] UTF-8 conversion failed, trying Latin-1");
            let text = bytes.iter().map(|&b| b as char).collect::<String>();
            Ok(text)
        }
    }
}
