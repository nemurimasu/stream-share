use quick_xml::events::Event as XmlEvent;
use std::ops::Range;
use yaml_rust::parser::{Event as YamlEvent, Parser as YamlParser};
use yaml_rust::scanner::{Marker as YamlMarker, ScanError as YamlError};

fn skip<R: Iterator<Item = char>>(parser: &mut YamlParser<R>) -> Result<(), YamlError> {
    loop {
        match parser.next()? {
            (YamlEvent::MappingStart(_), _) | (YamlEvent::SequenceStart(_), _) => skip(parser)?,
            (YamlEvent::MappingEnd, _)
            | (YamlEvent::SequenceEnd, _)
            | (YamlEvent::DocumentEnd, _)
            | (YamlEvent::StreamEnd, _) => break Ok(()),
            _ => {}
        }
    }
}

fn skip_to_next_map<R: Iterator<Item = char>>(
    parser: &mut YamlParser<R>,
) -> Result<Option<YamlMarker>, YamlError> {
    loop {
        match parser.next()? {
            (YamlEvent::MappingStart(_), marker) => break Ok(Some(marker)),
            (YamlEvent::SequenceStart(_), _) => skip(parser)?,
            (YamlEvent::MappingEnd, _)
            | (YamlEvent::SequenceEnd, _)
            | (YamlEvent::DocumentEnd, _)
            | (YamlEvent::StreamEnd, _) => break Ok(None),
            _ => {}
        }
    }
}

fn skip_to_map_value<R: Iterator<Item = char>>(
    parser: &mut YamlParser<R>,
    key: &str,
) -> Result<Option<YamlMarker>, YamlError> {
    loop {
        match parser.next()? {
            (YamlEvent::Scalar(ref v, _, _, _), pos) if v == key => break Ok(Some(pos)),
            (YamlEvent::MappingStart(_), _) | (YamlEvent::SequenceStart(_), _) => skip(parser)?,
            (YamlEvent::MappingEnd, _)
            | (YamlEvent::SequenceEnd, _)
            | (YamlEvent::DocumentEnd, _)
            | (YamlEvent::StreamEnd, _) => break Ok(None),
            _ => {}
        }
    }
}

struct File {
    path: Option<String>,
    content: Option<(String, Range<usize>)>,
}

fn read_file<R: Iterator<Item = char>>(parser: &mut YamlParser<R>) -> Result<File, YamlError> {
    let mut path = None;
    let mut content = None;
    loop {
        match parser.next()? {
            (YamlEvent::Scalar(k, _, _, _), _) => match parser.next()? {
                (YamlEvent::Scalar(v, _, _, _), pos) => {
                    if &k == "path" {
                        path = Some(v);
                    } else if &k == "content" {
                        let next = parser.peek()?.1;
                        content = Some((
                            v,
                            (Range {
                                start: pos.index(),
                                end: next.index() - next.col() - 1,
                            }),
                        ))
                    }
                }
                (YamlEvent::MappingStart(_), _) | (YamlEvent::SequenceStart(_), _) => skip(parser)?,
                _ => unreachable!("key without a value"),
            },
            (YamlEvent::MappingStart(_), _) | (YamlEvent::SequenceStart(_), _) => skip(parser)?,
            (YamlEvent::MappingEnd, _)
            | (YamlEvent::SequenceEnd, _)
            | (YamlEvent::DocumentEnd, _)
            | (YamlEvent::StreamEnd, _) => break Ok(File { path, content }),
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum IcecastPart<'a> {
    Text(&'a str),
    SourcePassword,
    RelayPassword,
    AdminPassword,
}

struct IcecastConfig {
    raw: String,
    marks: Vec<(usize, IcecastPart<'static>)>,
}

impl IcecastConfig {
    pub fn new(input: &str) -> Result<IcecastConfig, quick_xml::Error> {
        let mut reader = quick_xml::Reader::from_str(input);
        let mut buffer = Vec::new();
        reader.trim_text(true);

        let mut output = Vec::new();
        let mut writer = quick_xml::Writer::new(&mut output);
        let mut marks = Vec::new();
        let mut out_pos = 0;
        loop {
            match reader.read_event(&mut buffer)? {
                XmlEvent::Eof => break,
                XmlEvent::Comment(comment) => {
                    let comment = comment.unescaped().unwrap();
                    let comment = reader.decode(&*comment);
                    match &*comment {
                        "source-password" => marks.push((out_pos, IcecastPart::SourcePassword)),
                        "relay-password" => marks.push((out_pos, IcecastPart::RelayPassword)),
                        "admin-password" => marks.push((out_pos, IcecastPart::AdminPassword)),
                        _ => {}
                    }
                }
                event => {
                    out_pos += writer.write_event(event)?;
                }
            }
        }

        Ok(IcecastConfig {
            raw: unsafe { String::from_utf8_unchecked(output) },
            marks,
        })
    }
}

impl<'a> IntoIterator for &'a IcecastConfig {
    type Item = IcecastPart<'a>;
    type IntoIter = IcecastConfigParts<'a>;

    fn into_iter(self) -> Self::IntoIter {
        IcecastConfigParts {
            raw: &self.raw[..],
            marks: &self.marks[..],
            pos: 0,
        }
    }
}

struct IcecastConfigParts<'a> {
    raw: &'a str,
    marks: &'a [(usize, IcecastPart<'static>)],
    pos: usize,
}

impl<'a> Iterator for IcecastConfigParts<'a> {
    type Item = IcecastPart<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.pos / 2;
        if index > self.marks.len() {
            return None;
        }
        let result = match self.pos % 2 {
            0 if index == 0 => Some(IcecastPart::Text(&self.raw[0..self.marks[index].0])),
            0 if index == self.marks.len() => Some(IcecastPart::Text(
                &self.raw[self.marks[index - 1].0..self.raw.len()],
            )),
            0 => Some(IcecastPart::Text(
                &self.raw[self.marks[index - 1].0..self.marks[index].0],
            )),
            _ if index < self.marks.len() => Some(self.marks[index].1),
            _ => None,
        };
        self.pos += 1;
        result
    }
}

fn push_escaped_str(dest: &mut String, source: &str) {
    dest.reserve(source.len());
    let mut start = 0;
    for (i, &c) in source.as_bytes().iter().enumerate() {
        match c {
            b'\'' | b'\\' => {
                if start < i {
                    dest.push_str(&source[start..i])
                }
                dest.push('\\');
                start = i;
                dest.reserve(source.len() - start);
            }
            _ => {}
        }
    }
    if start < source.len() {
        dest.push_str(&source[start..source.len()]);
    }
}

fn generate_cloud_init() -> Result<String, YamlError> {
    let cloud_init = include_str!("cloud-init.yaml");
    let mut parser = yaml_rust::parser::Parser::new(cloud_init.chars());
    skip_to_next_map(&mut parser)?;
    skip_to_map_value(&mut parser, "write_files")?;
    match parser.next()?.0 {
        YamlEvent::SequenceStart(_) => {}
        _ => panic!("cloud-init.yaml does not contain files list"),
    }
    let icecast = loop {
        match parser.next()? {
            (YamlEvent::MappingStart(_), _) => {
                let File { path, content } = read_file(&mut parser)?;
                if let Some("/etc/icecast2/icecast.xml") = path.as_ref().map(String::as_str) {
                    break content;
                }
            }
            (YamlEvent::MappingEnd, _)
            | (YamlEvent::SequenceEnd, _)
            | (YamlEvent::DocumentEnd, _)
            | (YamlEvent::StreamEnd, _) => break None,
            _ => {}
        }
    };
    if let Some((icecast, pos)) = icecast {
        let icecast = IcecastConfig::new(&icecast).unwrap();

        let mut new_str = String::new();
        new_str.push_str("[base64(concat('");
        push_escaped_str(&mut new_str, &cloud_init[0..pos.start]);
        for part in &icecast {
            match part {
                IcecastPart::Text(text) => push_escaped_str(&mut new_str, text),
                IcecastPart::SourcePassword => {
                    new_str.push_str("',variables('sourcePassword'),'");
                }
                IcecastPart::RelayPassword => {
                    new_str.push_str("',variables('relayPassword'),'");
                }
                IcecastPart::AdminPassword => {
                    new_str.push_str("',variables('adminPassword'),'");
                }
            }
        }
        push_escaped_str(&mut new_str, &cloud_init[pos.end..cloud_init.len()]);
        new_str.push_str("'))]");
        return Ok(new_str);
    } else {
        panic!("icecast.xml not found in cloud-init.yaml");
    }
}

fn main() {
    let template = include_str!("azuredeploy.json");
    let template = serde_json::from_str::<serde_json::Value>(&template).unwrap();

    let mut selector = jsonpath_lib::SelectorMut::new();
    let template = selector
        .str_path("$.resources[?(@.name == 'vm')].properties.osProfile.customData")
        .unwrap()
        .value(template)
        .replace_with(&mut |_| serde_json::Value::String(generate_cloud_init().unwrap()))
        .unwrap()
        .take()
        .unwrap();

    serde_json::to_writer_pretty(std::io::stdout(), &template).unwrap();
}
