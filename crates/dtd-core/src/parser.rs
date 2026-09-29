//! Analyse des déclarations DTD et de la déclaration `<!DOCTYPE>`.
//!
//! L'analyse est tolérante : chaque déclaration invalide produit un
//! [`DtdProblem`] localisé et l'analyse reprend à la déclaration suivante.
//! Les références d'entités paramètres sont développées entre les
//! déclarations (le texte de remplacement devient une source
//! [`SourceKind::Replacement`] ou [`SourceKind::External`]) et à
//! l'intérieur des déclarations (hors littéraux, avec un espace de part et
//! d'autre comme le veut XML 1.0 §4.4.8) ; les étendues d'un texte développé
//! sont rapportées à la référence `%nom;`.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use xml_core::tags::{XmlMarkupKind, scan_markup, scan_tags};

use crate::{
    AttributeDecl, AttributeType, ContentParticle, ContentSpec, DefaultDecl, Dtd, DtdProblem,
    DtdProblemKind, DtdSource, ElementDecl, EntityDecl, EntityExpansion, EntityValue,
    ExpansionError, Location, MAX_ENTITY_DEPTH, MAX_ENTITY_EXPANSION, MAX_PARAMETER_EXPANSION,
    MAX_SOURCES, NotationDecl, Occurrence, PREDEFINED_ENTITIES, SourceId, SourceKind,
    content::ParticleKind,
    names::{is_name, scan_name_chars},
};

/// Profondeur maximale des groupes imbriqués d'un modèle de contenu.
const MAX_GROUP_DEPTH: usize = 64;

/// Échec de lecture d'une ressource externe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    pub message: String,
    /// Ressource distante (`http(s)`) jamais téléchargée.
    pub remote: bool,
}

/// Lecture des ressources externes (sous-ensemble externe, entités
/// paramètres externes) ; `base` est le chemin de la source déclarante.
pub trait ExternalLoader {
    fn load(
        &mut self,
        public: Option<&str>,
        system: &str,
        base: Option<&Path>,
    ) -> Result<(PathBuf, String), LoadError>;
}

/// Chargeur qui refuse toute ressource externe.
pub struct NoLoader;

impl ExternalLoader for NoLoader {
    fn load(
        &mut self,
        _public: Option<&str>,
        system: &str,
        _base: Option<&Path>,
    ) -> Result<(PathBuf, String), LoadError> {
        Err(LoadError {
            message: format!("ressource externe « {system} » non chargée"),
            remote: false,
        })
    }
}

/// Déclaration `<!DOCTYPE>` d'un document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doctype {
    /// Déclaration entière.
    pub range: Range<usize>,
    pub name: String,
    pub name_range: Range<usize>,
    /// Identifiant public et étendue (guillemets exclus).
    pub public_id: Option<(String, Range<usize>)>,
    /// Identifiant système et étendue (guillemets exclus).
    pub system_id: Option<(String, Range<usize>)>,
    /// Contenu du sous-ensemble interne, crochets exclus.
    pub internal_subset: Option<Range<usize>>,
}

/// Déclaration `<!DOCTYPE>` qui précède l'élément racine.
pub fn find_doctype(source: &str) -> Option<Doctype> {
    let first_tag = scan_tags(source)
        .first()
        .map_or(source.len(), |tag| tag.range.start);
    let markup = scan_markup(source)
        .into_iter()
        .take_while(|markup| markup.range.start < first_tag)
        .find(|markup| {
            markup.kind == XmlMarkupKind::Declaration
                && source[markup.content.clone()].starts_with("DOCTYPE")
        })?;
    parse_doctype(source, markup.range, markup.content)
}

fn parse_doctype(source: &str, range: Range<usize>, content: Range<usize>) -> Option<Doctype> {
    let bytes = source.as_bytes();
    let end = content.end;
    let start = skip_spaces(bytes, content.start + "DOCTYPE".len(), end);
    let name_range = start..scan_name_chars(source, start, end);
    let mut index = skip_spaces(bytes, name_range.end, end);
    let keyword_end = scan_name_chars(source, index, end);
    let mut public_id = None;
    let mut system_id = None;
    match &source[index..keyword_end] {
        "SYSTEM" => {
            index = keyword_end;
            if let Some((value, range, next)) = quoted(source, index, end) {
                system_id = Some((value, range));
                index = next;
            }
        }
        "PUBLIC" => {
            index = keyword_end;
            if let Some((value, range, next)) = quoted(source, index, end) {
                public_id = Some((value, range));
                index = next;
                if let Some((value, range, next)) = quoted(source, index, end) {
                    system_id = Some((value, range));
                    index = next;
                }
            }
        }
        _ => {}
    }
    index = skip_spaces(bytes, index, end);
    let internal_subset = (index < end && bytes[index] == b'[').then(|| {
        let start = index + 1;
        start..subset_end(bytes, start, end)
    });
    Some(Doctype {
        range,
        name: source[name_range.clone()].to_owned(),
        name_range,
        public_id,
        system_id,
        internal_subset,
    })
}

fn skip_spaces(bytes: &[u8], mut index: usize, end: usize) -> usize {
    while index < end && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

/// Littéral entre guillemets après des blancs : `(valeur, étendue, suite)`.
fn quoted(source: &str, from: usize, end: usize) -> Option<(String, Range<usize>, usize)> {
    let bytes = source.as_bytes();
    let index = skip_spaces(bytes, from, end);
    let quote = *bytes.get(index).filter(|_| index < end)?;
    if !matches!(quote, b'"' | b'\'') {
        return None;
    }
    let close = index
        + 1
        + bytes[index + 1..end]
            .iter()
            .position(|&byte| byte == quote)?;
    Some((
        source[index + 1..close].to_owned(),
        index + 1..close,
        close + 1,
    ))
}

/// Position du `]` qui ferme le sous-ensemble interne (hors littéraux,
/// commentaires et instructions de traitement), ou `end`.
fn subset_end(bytes: &[u8], mut index: usize, end: usize) -> usize {
    while index < end {
        match bytes[index] {
            b'"' | b'\'' => {
                let quote = bytes[index];
                index = bytes[index + 1..end]
                    .iter()
                    .position(|&byte| byte == quote)
                    .map_or(end, |offset| index + offset + 2);
            }
            b'<' if bytes[index..end].starts_with(b"<!--") => {
                index = find_after(bytes, index + 4, end, b"-->");
            }
            b'<' if bytes[index..end].starts_with(b"<?") => {
                index = find_after(bytes, index + 2, end, b"?>");
            }
            // Section conditionnelle (interdite ici, mais ses crochets ne
            // ferment pas le sous-ensemble).
            b'<' if bytes[index..end].starts_with(b"<![") => {
                index = section_end(bytes, index + 3, end).map_or(end, |close| close + 3);
            }
            b']' => return index,
            _ => index += 1,
        }
    }
    end
}

fn find_after(bytes: &[u8], from: usize, end: usize, needle: &[u8]) -> usize {
    bytes[from.min(end)..end]
        .windows(needle.len())
        .position(|window| window == needle)
        .map_or(end, |offset| from + offset + needle.len())
}

/// Analyse un fichier DTD (sous-ensemble externe) complet.
pub fn parse_dtd(text: &str, path: Option<PathBuf>, loader: &mut dyn ExternalLoader) -> Dtd {
    let mut builder = DtdBuilder::new(loader);
    let source = builder.add_document(text, path);
    builder.parse_external_text(source);
    builder.finish()
}

/// Grammaire d'un document d'instance : sous-ensemble interne (source 0 =
/// le document), puis sous-ensemble externe. `None` sans `<!DOCTYPE>`.
pub fn load_document_dtd(
    document: &str,
    path: Option<PathBuf>,
    loader: &mut dyn ExternalLoader,
) -> Option<(Doctype, Dtd)> {
    let doctype = find_doctype(document)?;
    let mut builder = DtdBuilder::new(loader);
    if !doctype.name.is_empty() {
        builder.dtd.doctype_name = Some(doctype.name.clone());
    }
    let source = builder.add_document(document, path);
    if let Some(subset) = &doctype.internal_subset {
        builder.parse_internal_subset(source, subset.clone());
    }
    if let Some((system, range)) = &doctype.system_id {
        builder.parse_external_subset(
            doctype
                .public_id
                .as_ref()
                .map(|(public, _)| public.as_str()),
            system,
            Location {
                source,
                range: range.clone(),
            },
        );
    }
    Some((doctype, builder.finish()))
}

/// Décode le contenu d'une référence de caractère (`#10`, `#x1F`).
pub(crate) fn decode_char_reference(reference: &str) -> Option<char> {
    let digits = reference.strip_prefix('#')?;
    let code = match digits.strip_prefix('x') {
        Some(hex) if !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
            u32::from_str_radix(hex, 16).ok()?
        }
        None if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) => {
            digits.parse().ok()?
        }
        _ => return None,
    };
    char::from_u32(code).filter(|&character| {
        matches!(character, '\t' | '\n' | '\r')
            || ('\u{20}'..='\u{D7FF}').contains(&character)
            || ('\u{E000}'..='\u{FFFD}').contains(&character)
            || character >= '\u{10000}'
    })
}

/// Commentaire normalisé en documentation (lignes rognées).
fn normalize_comment(text: &str) -> Option<String> {
    let text = text.lines().map(str::trim).collect::<Vec<_>>().join("\n");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Fin d'une déclaration `<!...>` : `(suite, fin du corps, fermée)`. Un `<`
/// hors littéral interrompt une déclaration non fermée.
fn declaration_end(bytes: &[u8], mut index: usize, end: usize) -> (usize, usize, bool) {
    while index < end {
        match bytes[index] {
            b'"' | b'\'' => {
                let quote = bytes[index];
                match bytes[index + 1..end].iter().position(|&byte| byte == quote) {
                    Some(offset) => index += offset + 2,
                    None => return (end, end, false),
                }
            }
            b'>' => return (index + 1, index, true),
            b'<' => return (index, index, false),
            _ => index += 1,
        }
    }
    (end, end, false)
}

/// Position du `]]>` qui ferme une section conditionnelle (sections
/// imbriquées comprises).
fn section_end(bytes: &[u8], mut index: usize, end: usize) -> Option<usize> {
    let mut depth = 0usize;
    while index < end {
        let rest = &bytes[index..end];
        if rest.starts_with(b"<!--") {
            index = find_after(bytes, index + 4, end, b"-->");
        } else if rest.starts_with(b"<![") {
            depth += 1;
            index += 3;
        } else if rest.starts_with(b"]]>") {
            if depth == 0 {
                return Some(index);
            }
            depth -= 1;
            index += 3;
        } else {
            index += 1;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Texte développé
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Origin {
    /// Recopié de la source à partir de cet offset.
    Verbatim(usize),
    /// Issu du développement de la référence `%nom;` à cette étendue.
    Reference(Range<usize>),
}

#[derive(Debug, Clone)]
struct Segment {
    start: usize,
    end: usize,
    origin: Origin,
}

/// Corps d'une déclaration après développement des entités paramètres.
#[derive(Debug, Default)]
struct Expanded {
    text: String,
    segments: Vec<Segment>,
    fallback: Range<usize>,
}

impl Expanded {
    fn push(&mut self, text: &str, origin: Option<&Range<usize>>, source_start: usize) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        self.segments.push(Segment {
            start,
            end: self.text.len(),
            origin: match origin {
                Some(reference) => Origin::Reference(reference.clone()),
                None => Origin::Verbatim(source_start),
            },
        });
    }

    /// Étendue de la source correspondant à `range` du texte développé.
    fn locate(&self, range: &Range<usize>) -> Range<usize> {
        let segment = self
            .segments
            .iter()
            .find(|segment| segment.start <= range.start && range.start < segment.end)
            .or_else(|| self.segments.last());
        match segment {
            Some(Segment {
                start,
                end,
                origin: Origin::Verbatim(offset),
            }) => {
                let from = offset + range.start.clamp(*start, *end) - start;
                let to = offset + range.end.clamp(*start, *end) - start;
                from..to.max(from)
            }
            Some(Segment {
                origin: Origin::Reference(reference),
                ..
            }) => reference.clone(),
            None => self.fallback.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Lexèmes d'une déclaration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Name,
    /// `#PCDATA`, `#REQUIRED`...
    Hash,
    Percent,
    Literal,
    UnclosedLiteral,
    Punct(u8),
    Other,
    End,
}

struct Lexer<'t> {
    text: &'t str,
    position: usize,
}

impl<'t> Lexer<'t> {
    fn new(text: &'t str) -> Self {
        Self { text, position: 0 }
    }

    fn next(&mut self) -> (Token, Range<usize>) {
        let bytes = self.text.as_bytes();
        while self.position < bytes.len() && bytes[self.position].is_ascii_whitespace() {
            self.position += 1;
        }
        let start = self.position;
        let Some(&byte) = bytes.get(start) else {
            return (Token::End, start..start);
        };
        let (token, end) = match byte {
            b'"' | b'\'' => match self.text[start + 1..].find(byte as char) {
                Some(offset) => (Token::Literal, start + offset + 2),
                None => (Token::UnclosedLiteral, bytes.len()),
            },
            b'#' => (
                Token::Hash,
                scan_name_chars(self.text, start + 1, bytes.len()),
            ),
            b'%' => (Token::Percent, start + 1),
            b'(' | b')' | b'|' | b',' | b'?' | b'*' | b'+' => (Token::Punct(byte), start + 1),
            _ => {
                let end = scan_name_chars(self.text, start, bytes.len());
                if end > start {
                    (Token::Name, end)
                } else {
                    let length = self.text[start..].chars().next().map_or(1, char::len_utf8);
                    (Token::Other, start + length)
                }
            }
        };
        self.position = end;
        (token, start..end)
    }

    fn peek(&self) -> (Token, Range<usize>) {
        Lexer {
            text: self.text,
            position: self.position,
        }
        .next()
    }
}

#[derive(Debug)]
struct ParseError {
    range: Range<usize>,
    message: String,
}

impl ParseError {
    fn new(range: Range<usize>, message: impl Into<String>) -> Self {
        Self {
            range,
            message: message.into(),
        }
    }
}

fn literal_content<'t>(
    token: Token,
    text: &'t str,
    range: &Range<usize>,
) -> Result<&'t str, ParseError> {
    match token {
        Token::Literal => Ok(&text[range.start + 1..range.end - 1]),
        Token::UnclosedLiteral => Err(ParseError::new(
            range.clone(),
            "littéral non fermé : guillemet fermant attendu",
        )),
        _ => Err(ParseError::new(
            range.clone(),
            "valeur entre guillemets attendue",
        )),
    }
}

fn parse_content_spec(lexer: &mut Lexer<'_>, text: &str) -> Result<ContentSpec, ParseError> {
    let (token, range) = lexer.next();
    match token {
        Token::Name if &text[range.clone()] == "EMPTY" => Ok(ContentSpec::Empty),
        Token::Name if &text[range.clone()] == "ANY" => Ok(ContentSpec::Any),
        Token::Punct(b'(') => {
            let (next, next_range) = lexer.peek();
            if next == Token::Hash {
                lexer.next();
                if &text[next_range.clone()] != "#PCDATA" {
                    return Err(ParseError::new(next_range, "« #PCDATA » attendu"));
                }
                parse_mixed(lexer, text)
            } else {
                parse_group(lexer, text, 0).map(ContentSpec::Children)
            }
        }
        _ => Err(ParseError::new(
            range,
            "modèle de contenu attendu : EMPTY, ANY ou « ( »",
        )),
    }
}

/// Suite d'un modèle mixte après `( #PCDATA`.
fn parse_mixed(lexer: &mut Lexer<'_>, text: &str) -> Result<ContentSpec, ParseError> {
    let mut names = Vec::new();
    loop {
        let (token, range) = lexer.next();
        match token {
            Token::Punct(b'|') => {
                let (token, range) = lexer.next();
                if token != Token::Name {
                    return Err(ParseError::new(range, "nom d'élément attendu après « | »"));
                }
                names.push(text[range].to_owned());
            }
            Token::Punct(b')') => break,
            _ => {
                return Err(ParseError::new(
                    range,
                    "« | » ou « ) » attendu dans un modèle mixte",
                ));
            }
        }
    }
    let (token, range) = lexer.peek();
    if token == Token::Punct(b'*') {
        lexer.next();
    } else if !names.is_empty() {
        return Err(ParseError::new(
            range,
            "« * » attendu après un modèle mixte qui cite des éléments",
        ));
    }
    Ok(ContentSpec::Mixed(names))
}

/// Groupe après `(`, jusqu'à `)` et sa cardinalité.
fn parse_group(
    lexer: &mut Lexer<'_>,
    text: &str,
    depth: usize,
) -> Result<ContentParticle, ParseError> {
    if depth >= MAX_GROUP_DEPTH {
        let (_, range) = lexer.peek();
        return Err(ParseError::new(range, "modèle de contenu trop imbriqué"));
    }
    let mut items = vec![parse_particle(lexer, text, depth)?];
    let mut separator = None;
    loop {
        let (token, range) = lexer.next();
        match token {
            Token::Punct(character @ (b',' | b'|')) => {
                if separator.is_some_and(|separator| separator != character) {
                    return Err(ParseError::new(
                        range,
                        "« , » et « | » ne peuvent pas être mélangés dans un même groupe",
                    ));
                }
                separator = Some(character);
                items.push(parse_particle(lexer, text, depth)?);
            }
            Token::Punct(b')') => break,
            _ => return Err(ParseError::new(range, "« , », « | » ou « ) » attendu")),
        }
    }
    let occurrence = parse_occurrence(lexer);
    Ok(ContentParticle {
        kind: if separator == Some(b'|') {
            ParticleKind::Choice(items)
        } else {
            ParticleKind::Sequence(items)
        },
        occurrence,
    })
}

fn parse_particle(
    lexer: &mut Lexer<'_>,
    text: &str,
    depth: usize,
) -> Result<ContentParticle, ParseError> {
    let (token, range) = lexer.next();
    match token {
        Token::Name => Ok(ContentParticle {
            kind: ParticleKind::Name(text[range].to_owned()),
            occurrence: parse_occurrence(lexer),
        }),
        Token::Punct(b'(') => parse_group(lexer, text, depth + 1),
        Token::Hash => Err(ParseError::new(
            range,
            "« #PCDATA » doit être le premier élément du groupe",
        )),
        _ => Err(ParseError::new(range, "nom d'élément ou « ( » attendu")),
    }
}

fn parse_occurrence(lexer: &mut Lexer<'_>) -> Occurrence {
    let occurrence = match lexer.peek().0 {
        Token::Punct(b'?') => Occurrence::Optional,
        Token::Punct(b'*') => Occurrence::ZeroOrMore,
        Token::Punct(b'+') => Occurrence::OneOrMore,
        _ => return Occurrence::Once,
    };
    lexer.next();
    occurrence
}

/// Valeurs d'une énumération après `(`, jusqu'à `)`.
fn parse_enumeration(
    lexer: &mut Lexer<'_>,
    text: &str,
    notation: bool,
) -> Result<Vec<String>, ParseError> {
    let mut values = Vec::new();
    loop {
        let (token, range) = lexer.next();
        if token != Token::Name {
            return Err(ParseError::new(
                range,
                if notation {
                    "nom de notation attendu"
                } else {
                    "valeur d'énumération (NMTOKEN) attendue"
                },
            ));
        }
        let value = &text[range.clone()];
        if notation && !is_name(value) {
            return Err(ParseError::new(
                range,
                format!("« {value} » n'est pas un nom de notation valide"),
            ));
        }
        values.push(value.to_owned());
        let (token, range) = lexer.next();
        match token {
            Token::Punct(b'|') => {}
            Token::Punct(b')') => return Ok(values),
            _ => return Err(ParseError::new(range, "« | » ou « ) » attendu")),
        }
    }
}

/// `SYSTEM "…"`, `PUBLIC "…" "…"` (ou `PUBLIC "…"` seul pour une notation)
/// après le mot-clé `keyword`.
fn external_id(
    lexer: &mut Lexer<'_>,
    text: &str,
    keyword: Range<usize>,
    public_only: bool,
) -> Result<(Option<String>, Option<String>), ParseError> {
    match &text[keyword.clone()] {
        "SYSTEM" => {
            let (token, range) = lexer.next();
            let system = literal_content(token, text, &range)?;
            Ok((None, Some(system.to_owned())))
        }
        "PUBLIC" => {
            let (token, range) = lexer.next();
            let public = literal_content(token, text, &range)?.to_owned();
            let (token, range) = lexer.peek();
            if matches!(token, Token::Literal | Token::UnclosedLiteral) {
                lexer.next();
                let system = literal_content(token, text, &range)?;
                Ok((Some(public), Some(system.to_owned())))
            } else if public_only {
                Ok((Some(public), None))
            } else {
                Err(ParseError::new(
                    range,
                    "identifiant système attendu après l'identifiant public",
                ))
            }
        }
        other => Err(ParseError::new(
            keyword,
            format!("SYSTEM ou PUBLIC attendu, « {other} » trouvé"),
        )),
    }
}

fn expect_end(lexer: &mut Lexer<'_>) -> Result<(), ParseError> {
    let (token, range) = lexer.next();
    if token == Token::End {
        Ok(())
    } else {
        Err(ParseError::new(
            range,
            "contenu inattendu : fin de déclaration « > » attendue",
        ))
    }
}

// ---------------------------------------------------------------------------
// Construction de la grammaire
// ---------------------------------------------------------------------------

/// Construit une [`Dtd`] à partir d'un ou plusieurs textes.
pub struct DtdBuilder<'l> {
    dtd: Dtd,
    loader: &'l mut dyn ExternalLoader,
    /// Entités paramètres en cours de développement.
    active: Vec<String>,
    /// Profondeur des sections conditionnelles.
    depth: usize,
    /// Octets de textes de remplacement développés.
    expanded: usize,
}

impl<'l> DtdBuilder<'l> {
    pub fn new(loader: &'l mut dyn ExternalLoader) -> Self {
        Self {
            dtd: Dtd::default(),
            loader,
            active: Vec::new(),
            depth: 0,
            expanded: 0,
        }
    }

    /// Ajoute le texte d'un document analysé directement.
    pub fn add_document(&mut self, text: &str, path: Option<PathBuf>) -> SourceId {
        self.push_source(SourceKind::Document(path), Arc::from(text), None)
    }

    /// Analyse le sous-ensemble interne `range` de `source`.
    pub fn parse_internal_subset(&mut self, source: SourceId, range: Range<usize>) {
        self.parse_declarations(source, range, false);
    }

    /// Analyse `source` entière comme sous-ensemble externe.
    pub fn parse_external_text(&mut self, source: SourceId) {
        let length = self.dtd.source_text(source).len();
        self.parse_declarations(source, 0..length, true);
    }

    /// Charge et analyse le sous-ensemble externe référencé en `reference`.
    pub fn parse_external_subset(
        &mut self,
        public: Option<&str>,
        system: &str,
        reference: Location,
    ) {
        let base = self.base_path(reference.source);
        if let Some(source) = self.load_external(public, system, base.as_deref(), &reference) {
            self.parse_external_text(source);
        }
    }

    pub fn finish(mut self) -> Dtd {
        self.check_references();
        self.compute_expansions();
        self.dtd
    }

    fn push_source(
        &mut self,
        kind: SourceKind,
        text: Arc<str>,
        reference: Option<Location>,
    ) -> SourceId {
        self.dtd.sources.push(DtdSource {
            kind,
            text,
            reference,
        });
        self.dtd.sources.len() - 1
    }

    fn problem(&mut self, kind: DtdProblemKind, location: Location, message: impl Into<String>) {
        self.dtd.problems.push(DtdProblem {
            kind,
            location,
            message: message.into(),
        });
    }

    fn at(source: SourceId, range: Range<usize>) -> Location {
        Location { source, range }
    }

    /// Chemin servant de base aux identifiants système relatifs.
    fn base_path(&self, source: SourceId) -> Option<PathBuf> {
        let anchored = self.dtd.anchor(&Self::at(source, 0..0));
        self.dtd.source_path(anchored.source).map(Path::to_path_buf)
    }

    fn sources_exhausted(&mut self, reference: &Location) -> bool {
        if self.dtd.sources.len() < MAX_SOURCES {
            return false;
        }
        self.dtd.incomplete = true;
        self.problem(
            DtdProblemKind::EntityExpansionLimit,
            reference.clone(),
            format!("trop de ressources DTD développées (limite {MAX_SOURCES})"),
        );
        true
    }

    fn load_external(
        &mut self,
        public: Option<&str>,
        system: &str,
        base: Option<&Path>,
        reference: &Location,
    ) -> Option<SourceId> {
        if self.sources_exhausted(reference) {
            return None;
        }
        match self.loader.load(public, system, base) {
            Ok((path, text)) => Some(self.push_source(
                SourceKind::External(path),
                Arc::from(text),
                Some(reference.clone()),
            )),
            Err(error) => {
                self.dtd.incomplete = true;
                self.problem(
                    DtdProblemKind::ExternalLoad {
                        remote: error.remote,
                    },
                    reference.clone(),
                    error.message,
                );
                None
            }
        }
    }

    /// Texte de remplacement de l'entité paramètre `name` référencée en
    /// `at`, et chemin du fichier pour une entité externe. Vérifie la
    /// déclaration, la récursion, la profondeur et le budget de
    /// développement.
    fn parameter_text(&mut self, name: &str, at: &Location) -> Option<(Arc<str>, Option<PathBuf>)> {
        let Some(value) = self
            .dtd
            .parameter_entity(name)
            .map(|entity| entity.value.clone())
        else {
            if !self.dtd.incomplete {
                self.problem(
                    DtdProblemKind::UndeclaredParameterEntity,
                    at.clone(),
                    format!("l'entité paramètre « %{name}; » n'est pas déclarée"),
                );
            }
            return None;
        };
        if self.active.iter().any(|active| active == name) {
            self.problem(
                DtdProblemKind::EntityRecursion,
                at.clone(),
                format!("référence récursive à l'entité paramètre « %{name}; »"),
            );
            return None;
        }
        if self.active.len() >= MAX_ENTITY_DEPTH {
            self.problem(
                DtdProblemKind::EntityExpansionLimit,
                at.clone(),
                format!(
                    "imbrication d'entités paramètres trop profonde (limite {MAX_ENTITY_DEPTH})"
                ),
            );
            return None;
        }
        let (text, path): (Arc<str>, Option<PathBuf>) = match value {
            EntityValue::Internal(text) => (Arc::from(text.as_str()), None),
            EntityValue::External {
                public,
                system,
                base,
                ..
            } => {
                if self.sources_exhausted(at) {
                    return None;
                }
                match self
                    .loader
                    .load(public.as_deref(), &system, base.as_deref())
                {
                    Ok((path, text)) => (Arc::from(text), Some(path)),
                    Err(error) => {
                        self.dtd.incomplete = true;
                        self.problem(
                            DtdProblemKind::ExternalLoad {
                                remote: error.remote,
                            },
                            at.clone(),
                            error.message,
                        );
                        return None;
                    }
                }
            }
        };
        self.expanded = self.expanded.saturating_add(text.len());
        if self.expanded > MAX_PARAMETER_EXPANSION {
            self.problem(
                DtdProblemKind::EntityExpansionLimit,
                at.clone(),
                format!(
                    "développement des entités paramètres trop volumineux (limite {MAX_PARAMETER_EXPANSION} octets)"
                ),
            );
            return None;
        }
        Some((text, path))
    }

    /// Déclarations, commentaires, instructions de traitement, sections
    /// conditionnelles et références `%nom;` de `range` dans `source`.
    fn parse_declarations(&mut self, source: SourceId, range: Range<usize>, external: bool) {
        let text = self.dtd.sources[source].text.clone();
        let bytes = text.as_bytes();
        let end = range.end.min(text.len());
        let mut index = range.start;
        let mut documentation: Option<String> = None;
        while index < end {
            let byte = bytes[index];
            if byte.is_ascii_whitespace() {
                index += 1;
                continue;
            }
            let rest = &text[index..end];
            if let Some(comment) = rest.strip_prefix("<!--") {
                match comment.find("-->") {
                    Some(offset) => {
                        documentation = normalize_comment(&comment[..offset]);
                        index += offset + 7;
                    }
                    None => {
                        self.problem(
                            DtdProblemKind::Syntax,
                            Self::at(source, index..end),
                            "commentaire non fermé : « --> » attendu",
                        );
                        return;
                    }
                }
                continue;
            }
            if rest.starts_with("<?") {
                match rest.find("?>") {
                    Some(offset) => index += offset + 2,
                    None => {
                        self.problem(
                            DtdProblemKind::Syntax,
                            Self::at(source, index..end),
                            "instruction de traitement non fermée : « ?> » attendu",
                        );
                        return;
                    }
                }
                documentation = None;
                continue;
            }
            if rest.starts_with("<![") {
                index = self.conditional_section(source, &text, index, end, external);
                documentation = None;
                continue;
            }
            if rest.starts_with("<!") {
                let keyword_end = scan_name_chars(&text, index + 2, end);
                let keyword = &text[index + 2..keyword_end];
                let (declaration_end, body_end, closed) = declaration_end(bytes, keyword_end, end);
                match keyword {
                    "ELEMENT" | "ATTLIST" | "ENTITY" | "NOTATION" => self.declaration(
                        source,
                        keyword,
                        index..declaration_end,
                        keyword_end..body_end,
                        documentation.take(),
                    ),
                    _ => self.problem(
                        DtdProblemKind::Syntax,
                        Self::at(source, index..keyword_end.max(index + 2)),
                        format!(
                            "déclaration inconnue « <!{keyword} » : ELEMENT, ATTLIST, ENTITY ou NOTATION attendu"
                        ),
                    ),
                }
                if !closed {
                    self.problem(
                        DtdProblemKind::Syntax,
                        Self::at(source, index..keyword_end.max(index + 2)),
                        "déclaration non fermée : « > » attendu",
                    );
                }
                documentation = None;
                index = declaration_end;
                continue;
            }
            if byte == b'%' {
                let name_end = scan_name_chars(&text, index + 1, end);
                if name_end > index + 1 && name_end < end && bytes[name_end] == b';' {
                    let name = text[index + 1..name_end].to_owned();
                    self.include_parameter_entity(source, &name, index..name_end + 1, external);
                    index = name_end + 1;
                } else {
                    self.problem(
                        DtdProblemKind::Syntax,
                        Self::at(source, index..name_end.max(index + 1)),
                        "référence d'entité paramètre invalide : « %nom; » attendu",
                    );
                    index = name_end.max(index + 1);
                }
                documentation = None;
                continue;
            }
            let stop = text[index + 1..end]
                .find(['<', '%'])
                .map_or(end, |offset| index + 1 + offset);
            let unexpected_end = index + text[index..stop].trim_end().len();
            self.problem(
                DtdProblemKind::Syntax,
                Self::at(source, index..unexpected_end.max(index + 1)),
                "contenu inattendu dans la DTD : déclaration « <!…> » attendue",
            );
            documentation = None;
            index = stop;
        }
    }

    /// Référence `%nom;` entre deux déclarations : son texte de
    /// remplacement est analysé comme une suite de déclarations.
    fn include_parameter_entity(
        &mut self,
        source: SourceId,
        name: &str,
        reference: Range<usize>,
        external: bool,
    ) {
        let at = Self::at(source, reference);
        let Some((text, path)) = self.parameter_text(name, &at) else {
            return;
        };
        if self.sources_exhausted(&at) {
            return;
        }
        let from_file = path.is_some();
        let kind = match path {
            Some(path) => SourceKind::External(path),
            None => SourceKind::Replacement {
                entity: name.to_owned(),
                reference: at.clone(),
            },
        };
        let length = text.len();
        let id = self.push_source(kind, text, Some(at));
        self.active.push(name.to_owned());
        self.parse_declarations(id, 0..length, external || from_file);
        self.active.pop();
    }

    fn conditional_section(
        &mut self,
        source: SourceId,
        text: &str,
        start: usize,
        end: usize,
        external: bool,
    ) -> usize {
        let bytes = text.as_bytes();
        let Some(open) = text[start + 3..end]
            .find('[')
            .map(|offset| start + 3 + offset)
        else {
            self.problem(
                DtdProblemKind::Syntax,
                Self::at(source, start..end),
                "section conditionnelle invalide : « [ » attendu",
            );
            return end;
        };
        let keyword_range = start + 3..open;
        let keyword = self.expand(source, keyword_range.clone()).text;
        let keyword = keyword.trim();
        let close = section_end(bytes, open + 1, end);
        let next = close.map_or(end, |close| close + 3);
        let opening = Self::at(source, start..open + 1);
        if close.is_none() {
            self.problem(
                DtdProblemKind::Syntax,
                opening.clone(),
                "section conditionnelle non fermée : « ]]> » attendu",
            );
        }
        if !external {
            self.problem(
                DtdProblemKind::ConditionalSection,
                opening,
                "les sections conditionnelles ne sont permises que dans le sous-ensemble externe",
            );
            return next;
        }
        match keyword {
            "INCLUDE" => {
                if self.depth >= MAX_ENTITY_DEPTH {
                    self.problem(
                        DtdProblemKind::EntityExpansionLimit,
                        opening,
                        "sections conditionnelles trop imbriquées",
                    );
                    return next;
                }
                self.depth += 1;
                self.parse_declarations(source, open + 1..close.unwrap_or(end), external);
                self.depth -= 1;
            }
            "IGNORE" => {}
            other => self.problem(
                DtdProblemKind::Syntax,
                Self::at(source, keyword_range),
                format!("INCLUDE ou IGNORE attendu, « {other} » trouvé"),
            ),
        }
        next
    }

    /// Développe les références `%nom;` (hors littéraux) de `range`.
    fn expand(&mut self, source: SourceId, range: Range<usize>) -> Expanded {
        let text = self.dtd.sources[source].text.clone();
        let mut expanded = Expanded {
            fallback: range.clone(),
            ..Expanded::default()
        };
        self.expand_into(
            &text[range.clone()],
            range.start,
            source,
            None,
            &mut expanded,
        );
        expanded
    }

    /// `origin` : `None` pour un texte recopié de la source à partir de
    /// `base`, sinon la référence dont le texte est issu.
    fn expand_into(
        &mut self,
        text: &str,
        base: usize,
        source: SourceId,
        origin: Option<&Range<usize>>,
        out: &mut Expanded,
    ) {
        let bytes = text.as_bytes();
        let mut index = 0;
        let mut copied = 0;
        while index < bytes.len() {
            match bytes[index] {
                quote @ (b'"' | b'\'') => {
                    index = bytes[index + 1..]
                        .iter()
                        .position(|&byte| byte == quote)
                        .map_or(bytes.len(), |offset| index + offset + 2);
                }
                b'%' => {
                    let name_end = scan_name_chars(text, index + 1, bytes.len());
                    if name_end == index + 1 || bytes.get(name_end) != Some(&b';') {
                        index += 1;
                        continue;
                    }
                    out.push(&text[copied..index], origin, base + copied);
                    let reference = origin.cloned().unwrap_or(base + index..base + name_end + 1);
                    let name = &text[index + 1..name_end];
                    let at = Self::at(source, reference.clone());
                    if let Some((replacement, _)) = self.parameter_text(name, &at) {
                        out.push(" ", Some(&reference), 0);
                        self.active.push(name.to_owned());
                        self.expand_into(&replacement, 0, source, Some(&reference), out);
                        self.active.pop();
                        out.push(" ", Some(&reference), 0);
                    }
                    index = name_end + 1;
                    copied = index;
                }
                _ => index += 1,
            }
        }
        out.push(&text[copied..], origin, base + copied);
    }

    /// Texte de remplacement d'une entité interne : références d'entités
    /// paramètres et de caractères développées.
    fn entity_value(&mut self, raw: &str, at: &Location) -> String {
        let bytes = raw.as_bytes();
        let mut value = String::with_capacity(raw.len());
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'%' => {
                    let name_end = scan_name_chars(raw, index + 1, raw.len());
                    if name_end > index + 1 && bytes.get(name_end) == Some(&b';') {
                        let name = &raw[index + 1..name_end];
                        if let Some((replacement, _)) = self.parameter_text(name, at) {
                            self.active.push(name.to_owned());
                            let expanded = self.entity_value(&replacement, at);
                            self.active.pop();
                            value.push_str(&expanded);
                        }
                        index = name_end + 1;
                    } else {
                        value.push('%');
                        index += 1;
                    }
                }
                b'&' if bytes.get(index + 1) == Some(&b'#') => {
                    let decoded = raw[index..].find(';').and_then(|end| {
                        decode_char_reference(&raw[index + 1..index + end])
                            .map(|character| (character, end))
                    });
                    match decoded {
                        Some((character, end)) => {
                            value.push(character);
                            index += end + 1;
                        }
                        None => {
                            self.problem(
                                DtdProblemKind::Syntax,
                                at.clone(),
                                "référence de caractère invalide dans la valeur d'entité",
                            );
                            value.push('&');
                            index += 1;
                        }
                    }
                }
                _ => {
                    let character = raw[index..].chars().next().unwrap_or_default();
                    value.push(character);
                    index += character.len_utf8().max(1);
                }
            }
        }
        value
    }

    fn declaration(
        &mut self,
        source: SourceId,
        keyword: &str,
        whole: Range<usize>,
        body: Range<usize>,
        documentation: Option<String>,
    ) {
        let expanded = self.expand(source, body.clone());
        let declaration = Self::at(source, whole.clone());
        let result = match keyword {
            "ELEMENT" => self.element_declaration(&expanded, &declaration, documentation),
            "ATTLIST" => self.attlist_declaration(&expanded, &declaration, documentation),
            "ENTITY" => self.entity_declaration(&expanded, &declaration, documentation),
            _ => self.notation_declaration(&expanded, &declaration, documentation),
        };
        if let Err(error) = result {
            let mut range = expanded.locate(&error.range);
            if range.is_empty() {
                // Erreur en fin de déclaration : le mot-clé est signalé.
                range = whole.start..body.start;
            }
            self.problem(
                DtdProblemKind::Syntax,
                Self::at(source, range),
                error.message,
            );
        }
    }

    fn located(expanded: &Expanded, declaration: &Location, range: &Range<usize>) -> Location {
        Self::at(declaration.source, expanded.locate(range))
    }

    fn check_name(&mut self, name: &str, location: &Location) {
        if !is_name(name) {
            self.problem(
                DtdProblemKind::Syntax,
                location.clone(),
                format!("« {name} » n'est pas un nom XML valide"),
            );
        }
    }

    fn element_declaration(
        &mut self,
        expanded: &Expanded,
        declaration: &Location,
        documentation: Option<String>,
    ) -> Result<(), ParseError> {
        let text = expanded.text.as_str();
        let mut lexer = Lexer::new(text);
        let (token, name_range) = lexer.next();
        if token != Token::Name {
            return Err(ParseError::new(
                name_range,
                "nom d'élément attendu après « <!ELEMENT »",
            ));
        }
        let name = text[name_range.clone()].to_owned();
        let location = Self::located(expanded, declaration, &name_range);
        self.check_name(&name, &location);
        let parsed = parse_content_spec(&mut lexer, text)
            .and_then(|content| expect_end(&mut lexer).map(|()| content));
        let (content, result) = match parsed {
            Ok(content) => (content, Ok(())),
            // Élément enregistré quand même pour éviter des erreurs en cascade.
            Err(error) => (ContentSpec::Any, Err(error)),
        };
        if self.dtd.element_index.contains_key(&name) {
            self.problem(
                DtdProblemKind::DuplicateElement,
                location,
                format!("l'élément « {name} » est déjà déclaré"),
            );
        } else {
            self.dtd
                .element_index
                .insert(name.clone(), self.dtd.elements.len());
            self.dtd.elements.push(ElementDecl {
                name,
                location,
                declaration: declaration.clone(),
                content,
                documentation,
            });
        }
        result
    }

    fn attlist_declaration(
        &mut self,
        expanded: &Expanded,
        declaration: &Location,
        documentation: Option<String>,
    ) -> Result<(), ParseError> {
        let text = expanded.text.as_str();
        let mut lexer = Lexer::new(text);
        let (token, element_range) = lexer.next();
        if token != Token::Name {
            return Err(ParseError::new(
                element_range,
                "nom d'élément attendu après « <!ATTLIST »",
            ));
        }
        let element = text[element_range].to_owned();
        loop {
            let (token, name_range) = lexer.next();
            match token {
                Token::End => return Ok(()),
                Token::Name => {}
                _ => {
                    return Err(ParseError::new(
                        name_range,
                        "nom d'attribut ou fin de déclaration « > » attendu",
                    ));
                }
            }
            let name = text[name_range.clone()].to_owned();
            let (token, type_range) = lexer.next();
            let attribute_type = match token {
                Token::Name => match &text[type_range.clone()] {
                    "CDATA" => AttributeType::CData,
                    "ID" => AttributeType::Id,
                    "IDREF" => AttributeType::IdRef,
                    "IDREFS" => AttributeType::IdRefs,
                    "ENTITY" => AttributeType::Entity,
                    "ENTITIES" => AttributeType::Entities,
                    "NMTOKEN" => AttributeType::NmToken,
                    "NMTOKENS" => AttributeType::NmTokens,
                    "NOTATION" => {
                        let (token, range) = lexer.next();
                        if token != Token::Punct(b'(') {
                            return Err(ParseError::new(range, "« ( » attendu après NOTATION"));
                        }
                        AttributeType::Notation(parse_enumeration(&mut lexer, text, true)?)
                    }
                    other => {
                        return Err(ParseError::new(
                            type_range.clone(),
                            format!(
                                "type d'attribut inconnu « {other} » : CDATA, ID, IDREF, IDREFS, ENTITY, ENTITIES, NMTOKEN, NMTOKENS, NOTATION ou énumération attendu"
                            ),
                        ));
                    }
                },
                Token::Punct(b'(') => {
                    AttributeType::Enumeration(parse_enumeration(&mut lexer, text, false)?)
                }
                _ => {
                    return Err(ParseError::new(
                        type_range,
                        format!("type attendu pour l'attribut « {name} »"),
                    ));
                }
            };
            let cdata = attribute_type == AttributeType::CData;
            let (token, default_range) = lexer.next();
            let default = match token {
                Token::Hash => match &text[default_range.clone()] {
                    "#REQUIRED" => DefaultDecl::Required,
                    "#IMPLIED" => DefaultDecl::Implied,
                    "#FIXED" => {
                        let (token, range) = lexer.next();
                        let raw = literal_content(token, text, &range)?;
                        let location = Self::located(expanded, declaration, &range);
                        DefaultDecl::Fixed(self.default_value(raw, cdata, &location))
                    }
                    other => {
                        return Err(ParseError::new(
                            default_range.clone(),
                            format!("#REQUIRED, #IMPLIED ou #FIXED attendu, « {other} » trouvé"),
                        ));
                    }
                },
                Token::Literal | Token::UnclosedLiteral => {
                    let raw = literal_content(token, text, &default_range)?;
                    let location = Self::located(expanded, declaration, &default_range);
                    DefaultDecl::Default(self.default_value(raw, cdata, &location))
                }
                _ => {
                    return Err(ParseError::new(
                        default_range,
                        format!(
                            "valeur par défaut attendue pour l'attribut « {name} » : #REQUIRED, #IMPLIED, #FIXED ou valeur"
                        ),
                    ));
                }
            };
            let location = Self::located(expanded, declaration, &name_range);
            self.check_name(&name, &location);
            self.add_attribute(AttributeDecl {
                element: element.clone(),
                name,
                location,
                declaration: declaration.clone(),
                attribute_type,
                default,
                documentation: documentation.clone(),
            });
        }
    }

    /// Valeur par défaut normalisée d'un attribut.
    fn default_value(&mut self, raw: &str, cdata: bool, location: &Location) -> String {
        if raw.contains('<') {
            self.problem(
                DtdProblemKind::Syntax,
                location.clone(),
                "« < » est interdit dans une valeur d'attribut",
            );
        }
        self.dtd
            .normalize_attribute_value(raw, cdata)
            .unwrap_or_else(|| raw.to_owned())
    }

    fn add_attribute(&mut self, attribute: AttributeDecl) {
        let mut has_id = false;
        for other in self.dtd.attributes_of(&attribute.element) {
            if other.name == attribute.name {
                // La première déclaration d'un attribut l'emporte.
                return;
            }
            has_id |= other.attribute_type == AttributeType::Id;
        }
        if attribute.attribute_type == AttributeType::Id {
            if has_id {
                self.problem(
                    DtdProblemKind::MultipleIdAttributes,
                    attribute.location.clone(),
                    format!(
                        "l'élément « {} » a déjà un attribut de type ID",
                        attribute.element
                    ),
                );
            }
            if attribute.default.value().is_some() {
                self.problem(
                    DtdProblemKind::IdAttributeDefault,
                    attribute.location.clone(),
                    format!(
                        "l'attribut ID « {} » doit être #IMPLIED ou #REQUIRED",
                        attribute.name
                    ),
                );
            }
        }
        let index = self.dtd.attributes.len();
        self.dtd
            .attribute_index
            .entry(attribute.element.clone())
            .or_default()
            .push(index);
        self.dtd.attributes.push(attribute);
    }

    fn entity_declaration(
        &mut self,
        expanded: &Expanded,
        declaration: &Location,
        documentation: Option<String>,
    ) -> Result<(), ParseError> {
        let text = expanded.text.as_str();
        let mut lexer = Lexer::new(text);
        let (mut token, mut name_range) = lexer.next();
        let parameter = token == Token::Percent;
        if parameter {
            (token, name_range) = lexer.next();
        }
        if token != Token::Name {
            return Err(ParseError::new(name_range, "nom d'entité attendu"));
        }
        let name = text[name_range.clone()].to_owned();
        let location = Self::located(expanded, declaration, &name_range);
        self.check_name(&name, &location);
        let (token, value_range) = lexer.next();
        let value = match token {
            Token::Literal => {
                let raw = &text[value_range.start + 1..value_range.end - 1];
                let at = Self::located(expanded, declaration, &value_range);
                EntityValue::Internal(self.entity_value(raw, &at))
            }
            Token::Name => {
                let (public, system) = external_id(&mut lexer, text, value_range, false)?;
                let (token, range) = lexer.peek();
                let notation = if token == Token::Name && &text[range.clone()] == "NDATA" {
                    lexer.next();
                    if parameter {
                        return Err(ParseError::new(
                            range,
                            "NDATA est interdit pour une entité paramètre",
                        ));
                    }
                    let (token, notation_range) = lexer.next();
                    if token != Token::Name {
                        return Err(ParseError::new(
                            notation_range,
                            "nom de notation attendu après NDATA",
                        ));
                    }
                    Some(text[notation_range].to_owned())
                } else {
                    None
                };
                EntityValue::External {
                    public,
                    system: system.unwrap_or_default(),
                    notation,
                    base: self.base_path(declaration.source),
                }
            }
            Token::UnclosedLiteral => {
                return Err(ParseError::new(
                    value_range,
                    "littéral non fermé : guillemet fermant attendu",
                ));
            }
            _ => {
                return Err(ParseError::new(
                    value_range,
                    "valeur entre guillemets, SYSTEM ou PUBLIC attendu",
                ));
            }
        };
        let end = expect_end(&mut lexer);
        let entity = EntityDecl {
            name: name.clone(),
            parameter,
            location,
            declaration: declaration.clone(),
            value,
            documentation,
            expansion: EntityExpansion::default(),
        };
        // La première déclaration d'une entité l'emporte.
        if parameter {
            if !self.dtd.parameter_index.contains_key(&name) {
                self.dtd
                    .parameter_index
                    .insert(name, self.dtd.parameter_entities.len());
                self.dtd.parameter_entities.push(entity);
            }
        } else if !self.dtd.general_index.contains_key(&name) {
            self.dtd
                .general_index
                .insert(name, self.dtd.general_entities.len());
            self.dtd.general_entities.push(entity);
        }
        end
    }

    fn notation_declaration(
        &mut self,
        expanded: &Expanded,
        declaration: &Location,
        documentation: Option<String>,
    ) -> Result<(), ParseError> {
        let text = expanded.text.as_str();
        let mut lexer = Lexer::new(text);
        let (token, name_range) = lexer.next();
        if token != Token::Name {
            return Err(ParseError::new(
                name_range,
                "nom de notation attendu après « <!NOTATION »",
            ));
        }
        let name = text[name_range.clone()].to_owned();
        let location = Self::located(expanded, declaration, &name_range);
        self.check_name(&name, &location);
        let (token, keyword) = lexer.next();
        if token != Token::Name {
            return Err(ParseError::new(keyword, "SYSTEM ou PUBLIC attendu"));
        }
        let (public, system) = external_id(&mut lexer, text, keyword, true)?;
        let end = expect_end(&mut lexer);
        if self.dtd.notation_index.contains_key(&name) {
            self.problem(
                DtdProblemKind::DuplicateNotation,
                location,
                format!("la notation « {name} » est déjà déclarée"),
            );
        } else {
            self.dtd
                .notation_index
                .insert(name.clone(), self.dtd.notations.len());
            self.dtd.notations.push(NotationDecl {
                name,
                location,
                declaration: declaration.clone(),
                public,
                system,
                documentation,
            });
        }
        end
    }

    /// Notations des entités `NDATA` et valeurs par défaut énumérées.
    fn check_references(&mut self) {
        let mut problems = Vec::new();
        if !self.dtd.incomplete {
            for entity in &self.dtd.general_entities {
                if let EntityValue::External {
                    notation: Some(notation),
                    ..
                } = &entity.value
                    && self.dtd.notation(notation).is_none()
                {
                    problems.push((
                        DtdProblemKind::UndeclaredNotation,
                        entity.location.clone(),
                        format!(
                            "la notation « {notation} » de l'entité « {} » n'est pas déclarée",
                            entity.name
                        ),
                    ));
                }
            }
        }
        for attribute in &self.dtd.attributes {
            if let (Some(values), Some(value)) =
                (attribute.attribute_type.values(), attribute.default.value())
                && !values.iter().any(|allowed| allowed == value)
            {
                problems.push((
                    DtdProblemKind::InvalidDefaultValue,
                    attribute.location.clone(),
                    format!(
                        "la valeur par défaut « {value} » de l'attribut « {} » n'est pas dans l'énumération ({})",
                        attribute.name,
                        values.join(" | ")
                    ),
                ));
            }
        }
        for (kind, location, message) in problems {
            self.problem(kind, location, message);
        }
    }

    /// Calcule le développement de chaque entité générale sans le
    /// matérialiser (tailles mémoïsées, saturées).
    fn compute_expansions(&mut self) {
        let count = self.dtd.general_entities.len();
        let mut computed: Vec<Option<EntityExpansion>> = vec![None; count];
        let mut stack = Vec::new();
        for index in 0..count {
            self.expansion(index, &mut computed, &mut stack);
        }
        for (entity, expansion) in self.dtd.general_entities.iter_mut().zip(computed) {
            entity.expansion = expansion.unwrap_or_default();
        }
    }

    fn expansion(
        &mut self,
        index: usize,
        computed: &mut Vec<Option<EntityExpansion>>,
        stack: &mut Vec<usize>,
    ) -> EntityExpansion {
        if let Some(expansion) = &computed[index] {
            return expansion.clone();
        }
        let entity = &self.dtd.general_entities[index];
        let text = match &entity.value {
            EntityValue::External { notation, .. } => {
                let expansion = EntityExpansion {
                    external: true,
                    unparsed: notation.is_some(),
                    ..EntityExpansion::default()
                };
                computed[index] = Some(expansion.clone());
                return expansion;
            }
            EntityValue::Internal(text) => text.clone(),
        };
        let name = entity.name.clone();
        let location = entity.location.clone();
        let mut result = EntityExpansion {
            blank: true,
            markup: text.contains('<'),
            ..EntityExpansion::default()
        };
        let mut own_problem = None;
        if stack.len() >= MAX_ENTITY_DEPTH {
            result.error = Some(ExpansionError::TooLarge);
            own_problem = Some((
                DtdProblemKind::EntityExpansionLimit,
                format!(
                    "imbrication des entités trop profonde depuis « {name} » (limite {MAX_ENTITY_DEPTH})"
                ),
            ));
        } else {
            stack.push(index);
            let mut rest = text.as_str();
            while !rest.is_empty() {
                let (plain, reference, tail) = match rest.find('&') {
                    Some(position) => {
                        let after = &rest[position + 1..];
                        match after.find(';') {
                            Some(end) => {
                                (&rest[..position], Some(&after[..end]), &after[end + 1..])
                            }
                            None => (rest, None, ""),
                        }
                    }
                    None => (rest, None, ""),
                };
                result.length = result.length.saturating_add(plain.len());
                result.blank &= plain
                    .chars()
                    .all(|character| character.is_ascii_whitespace());
                rest = tail;
                let Some(reference) = reference else {
                    continue;
                };
                if reference.starts_with('#')
                    || PREDEFINED_ENTITIES
                        .iter()
                        .any(|(predefined, _)| *predefined == reference)
                {
                    result.length = result.length.saturating_add(1);
                    result.blank = false;
                    continue;
                }
                let Some(&nested) = self.dtd.general_index.get(reference) else {
                    result.length = result.length.saturating_add(reference.len() + 2);
                    result.blank = false;
                    continue;
                };
                if stack.contains(&nested) {
                    result.error = Some(ExpansionError::Recursive);
                    own_problem = Some((
                        DtdProblemKind::EntityRecursion,
                        format!(
                            "l'entité « {name} » se référence elle-même (via « &{reference}; »)"
                        ),
                    ));
                    break;
                }
                let inner = self.expansion(nested, computed, stack);
                result.length = result.length.saturating_add(inner.length);
                result.blank &= inner.blank;
                result.markup |= inner.markup;
                result.external |= inner.external;
                if inner.unparsed {
                    result.markup = true;
                }
                if inner.error.is_some() {
                    result.error = inner.error;
                    break;
                }
                if result.length > MAX_ENTITY_EXPANSION {
                    break;
                }
            }
            stack.pop();
            if result.error.is_none() && result.length > MAX_ENTITY_EXPANSION {
                result.error = Some(ExpansionError::TooLarge);
                own_problem = Some((
                    DtdProblemKind::EntityExpansionLimit,
                    format!(
                        "le développement de l'entité « {name} » dépasse la limite de {MAX_ENTITY_EXPANSION} octets (expansion exponentielle d'entités)"
                    ),
                ));
            }
        }
        if let Some((kind, message)) = own_problem {
            self.problem(kind, location, message);
        }
        computed[index] = Some(result.clone());
        result
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Chargeur de test : fichiers virtuels par identifiant système.
    #[derive(Default)]
    struct MemoryLoader {
        files: HashMap<String, String>,
        requests: Vec<(Option<String>, String, Option<PathBuf>)>,
    }

    impl ExternalLoader for MemoryLoader {
        fn load(
            &mut self,
            public: Option<&str>,
            system: &str,
            base: Option<&Path>,
        ) -> Result<(PathBuf, String), LoadError> {
            self.requests.push((
                public.map(str::to_owned),
                system.to_owned(),
                base.map(Path::to_path_buf),
            ));
            if system.starts_with("http") {
                return Err(LoadError {
                    message: format!("DTD distante « {system} »"),
                    remote: true,
                });
            }
            self.files
                .get(system)
                .map(|text| (PathBuf::from(format!("/dtd/{system}")), text.clone()))
                .ok_or_else(|| LoadError {
                    message: format!("fichier « {system} » introuvable"),
                    remote: false,
                })
        }
    }

    fn parse(text: &str) -> Dtd {
        parse_dtd(text, Some(PathBuf::from("/dtd/main.dtd")), &mut NoLoader)
    }

    fn problem_ids(dtd: &Dtd) -> Vec<&'static str> {
        dtd.problems
            .iter()
            .map(|problem| problem.kind.id())
            .collect()
    }

    fn text_at<'d>(dtd: &'d Dtd, location: &Location) -> &'d str {
        &dtd.source_text(location.source)[location.range.clone()]
    }

    #[test]
    fn parses_element_declarations_and_content_models() {
        let dtd = parse(
            "<!-- Un livre. -->\n<!ELEMENT book (title, (chapter | appendix)+, index?)>\n<!ELEMENT title (#PCDATA)>\n<!ELEMENT p (#PCDATA|em | strong)*>\n<!ELEMENT br EMPTY>\n<!ELEMENT any ANY>\n<!ELEMENT one (item)>",
        );
        assert!(dtd.problems.is_empty(), "{:?}", dtd.problems);
        let book = dtd.element("book").expect("book should be declared");
        assert_eq!(
            book.content.to_string(),
            "(title, (chapter | appendix)+, index?)"
        );
        assert_eq!(book.documentation.as_deref(), Some("Un livre."));
        assert_eq!(text_at(&dtd, &book.location), "book");
        assert!(text_at(&dtd, &book.declaration).starts_with("<!ELEMENT book"));
        assert_eq!(
            dtd.element("title").unwrap().content,
            ContentSpec::Mixed(Vec::new())
        );
        assert_eq!(
            dtd.element("p").unwrap().content,
            ContentSpec::Mixed(vec!["em".to_owned(), "strong".to_owned()])
        );
        assert_eq!(dtd.element("br").unwrap().content, ContentSpec::Empty);
        assert_eq!(dtd.element("any").unwrap().content, ContentSpec::Any);
        assert_eq!(dtd.element("one").unwrap().content.to_string(), "(item)");
        // La documentation ne s'applique qu'à la déclaration suivante.
        assert_eq!(dtd.element("title").unwrap().documentation, None);
    }

    #[test]
    fn parses_attribute_lists() {
        let dtd = parse(
            "<!ENTITY company \"ACME\">\n<!NOTATION gif SYSTEM \"image/gif\">\n<!-- Attributs communs. -->\n<!ATTLIST item\n  id ID #REQUIRED\n  ref IDREF #IMPLIED\n  refs IDREFS #IMPLIED\n  kind (a|b | c) \"b\"\n  version CDATA #FIXED \"1.0\"\n  owner CDATA '&company; Inc'\n  tokens NMTOKENS #IMPLIED\n  token NMTOKEN #IMPLIED\n  logo ENTITY #IMPLIED\n  logos ENTITIES #IMPLIED\n  format NOTATION (gif) #IMPLIED>\n<!ATTLIST item kind CDATA #IMPLIED extra CDATA #IMPLIED>",
        );
        assert!(dtd.problems.is_empty(), "{:?}", dtd.problems);
        let names = dtd
            .attributes_of("item")
            .map(|attribute| attribute.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "id", "ref", "refs", "kind", "version", "owner", "tokens", "token", "logo",
                "logos", "format", "extra"
            ]
        );
        let kind = dtd.attribute("item", "kind").unwrap();
        // Première déclaration de `kind` conservée.
        assert_eq!(
            kind.attribute_type,
            AttributeType::Enumeration(vec!["a".into(), "b".into(), "c".into()])
        );
        assert_eq!(kind.default, DefaultDecl::Default("b".into()));
        assert_eq!(kind.documentation.as_deref(), Some("Attributs communs."));
        assert_eq!(kind.display(), "<!ATTLIST item kind (a | b | c) \"b\">");
        assert_eq!(
            dtd.attribute("item", "version").unwrap().default,
            DefaultDecl::Fixed("1.0".into())
        );
        assert_eq!(
            dtd.attribute("item", "owner").unwrap().default,
            DefaultDecl::Default("ACME Inc".into())
        );
        assert_eq!(
            dtd.attribute("item", "format")
                .unwrap()
                .attribute_type
                .to_string(),
            "NOTATION (gif)"
        );
        assert_eq!(
            text_at(&dtd, &dtd.attribute("item", "id").unwrap().location),
            "id"
        );
    }

    #[test]
    fn parses_entities_and_notations() {
        let dtd = parse(
            "<!ENTITY % inline \"em | strong\">\n<!ENTITY copy \"&#169; &#x41;\">\n<!ENTITY chapter SYSTEM \"chapter.xml\">\n<!ENTITY logo PUBLIC \"-//ACME//Logo\" \"logo.gif\" NDATA gif>\n<!ENTITY amp \"&#38;#38;\">\n<!NOTATION gif PUBLIC \"-//GIF\">\n<!NOTATION png SYSTEM \"image/png\">\n<!ENTITY inline-copy \"%inline;\">",
        );
        assert!(dtd.problems.is_empty(), "{:?}", dtd.problems);
        let inline = dtd.parameter_entity("inline").unwrap();
        assert_eq!(inline.value, EntityValue::Internal("em | strong".into()));
        assert_eq!(inline.display(), "<!ENTITY % inline \"em | strong\">");
        assert_eq!(
            dtd.general_entity("copy").unwrap().value,
            EntityValue::Internal("© A".into())
        );
        assert_eq!(
            dtd.general_entity("inline-copy").unwrap().value,
            EntityValue::Internal("em | strong".into())
        );
        let chapter = dtd.general_entity("chapter").unwrap();
        assert!(chapter.expansion.external);
        assert!(!chapter.expansion.unparsed);
        assert_eq!(
            chapter.value,
            EntityValue::External {
                public: None,
                system: "chapter.xml".into(),
                notation: None,
                base: Some(PathBuf::from("/dtd/main.dtd")),
            }
        );
        let logo = dtd.general_entity("logo").unwrap();
        assert!(logo.expansion.unparsed);
        assert_eq!(
            logo.display(),
            "<!ENTITY logo PUBLIC \"-//ACME//Logo\" \"logo.gif\" NDATA gif>"
        );
        assert_eq!(
            dtd.notation("gif").unwrap().public.as_deref(),
            Some("-//GIF")
        );
        assert_eq!(dtd.notation("gif").unwrap().system, None);
        assert_eq!(
            dtd.general_entity("amp").unwrap().value,
            EntityValue::Internal("&#38;".into())
        );
        assert!(dtd.general_entity("parameter").is_none());
    }

    #[test]
    fn expands_parameter_entities_in_and_between_declarations() {
        let dtd = parse(
            "<!ENTITY % inline \"em | strong\">\n<!ENTITY % common \"id ID #IMPLIED class CDATA #IMPLIED\">\n<!ENTITY % decls \"<!ELEMENT em (#PCDATA)> <!ELEMENT strong (#PCDATA)>\">\n<!ELEMENT p (#PCDATA | %inline;)*>\n<!ATTLIST p %common; lang NMTOKEN #IMPLIED>\n%decls;",
        );
        assert!(dtd.problems.is_empty(), "{:?}", dtd.problems);
        assert_eq!(
            dtd.element("p").unwrap().content,
            ContentSpec::Mixed(vec!["em".into(), "strong".into()])
        );
        let class = dtd.attribute("p", "class").unwrap();
        // Nom issu d'une entité : localisé sur la référence `%common;`.
        assert_eq!(text_at(&dtd, &class.location), "%common;");
        let lang = dtd.attribute("p", "lang").unwrap();
        assert_eq!(text_at(&dtd, &lang.location), "lang");
        let em = dtd.element("em").unwrap();
        // Déclaration issue d'un texte de remplacement : ancrée sur `%decls;`.
        assert!(matches!(
            dtd.sources[em.location.source].kind,
            SourceKind::Replacement { .. }
        ));
        assert_eq!(text_at(&dtd, &dtd.anchor(&em.location)), "%decls;");
    }

    #[test]
    fn handles_conditional_sections_in_external_subsets() {
        let dtd = parse(
            "<!ENTITY % draft \"INCLUDE\">\n<!ENTITY % final \"IGNORE\">\n<![%draft;[\n  <!ELEMENT note (#PCDATA)>\n  <![ IGNORE [ <!ELEMENT nested EMPTY> ]]>\n]]>\n<![%final;[ <!ELEMENT hidden EMPTY> <![INCLUDE[ <!ELEMENT deep EMPTY> ]]> ]]>\n<![ INCLUDE [ <!ELEMENT shown EMPTY> ]]>",
        );
        assert!(dtd.problems.is_empty(), "{:?}", dtd.problems);
        assert!(dtd.element("note").is_some());
        assert!(dtd.element("shown").is_some());
        assert!(dtd.element("nested").is_none());
        assert!(dtd.element("hidden").is_none());
        assert!(dtd.element("deep").is_none());

        // Interdites dans le sous-ensemble interne.
        let document = "<!DOCTYPE r [ <![INCLUDE[ <!ELEMENT r EMPTY> ]]> ]><r/>";
        let (_, dtd) = load_document_dtd(document, None, &mut NoLoader).unwrap();
        assert_eq!(problem_ids(&dtd), vec!["conditionalSection"]);
        assert!(dtd.element("r").is_none());
    }

    #[test]
    fn reports_syntax_errors_and_recovers() {
        let source = "<!ELEMENT a (b, c | d)>\n<!ELEMENT b (#PCDATA | x)>\n<!ELEMENT c EMPTY>\n<!ELEMENT c ANY>\n<!ATTLIST c kind (x|y) \"z\" id ID #IMPLIED key ID \"k\">\n<!ENTITY e>\n<!FOO bar>\nstray text\n<!ENTITY % p \"x\"> %missing;\n<!ELEMENT d (#PCDATA)>\n<!ATTLIST d a BOGUS #IMPLIED>\n<!ENTITY u SYSTEM \"u.bin\" NDATA nowhere>\n<!ELEMENT 1bad EMPTY>\n<!ELEMENT open (x";
        let dtd = parse(source);
        let ids = problem_ids(&dtd);
        assert_eq!(
            ids,
            vec![
                "dtdSyntax",            // , et | mélangés
                "dtdSyntax",            // * manquant après le modèle mixte
                "duplicateElement",     // c
                "multipleIdAttributes", // key
                "idAttributeDefault",   // key "k"
                "dtdSyntax",            // <!ENTITY e>
                "dtdSyntax",            // <!FOO
                "dtdSyntax",            // stray text
                "undeclaredParameterEntity",
                "dtdSyntax", // BOGUS
                "dtdSyntax", // 1bad
                "dtdSyntax", // modèle non terminé
                "dtdSyntax", // déclaration non fermée
                "undeclaredNotation",
                "invalidDefaultValue", // "z"
            ],
            "{:#?}",
            dtd.problems
        );
        // Les déclarations valides restent disponibles.
        assert!(dtd.element("d").is_some());
        // Les déclarations invalides d'élément sont enregistrées (ANY).
        assert_eq!(dtd.element("a").unwrap().content, ContentSpec::Any);
        let mixed = &dtd.problems[0];
        assert_eq!(text_at(&dtd, &mixed.location), "|");
        let stray = &dtd.problems[7];
        assert_eq!(text_at(&dtd, &stray.location), "stray text");
        let undeclared = &dtd.problems[8];
        assert_eq!(text_at(&dtd, &undeclared.location), "%missing;");
    }

    #[test]
    fn detects_parameter_entity_recursion() {
        // `&#37;` devient `%` dans le texte de remplacement : `%b;` se
        // référence lui-même une fois développé.
        let dtd = parse("<!ENTITY % b \"<!ELEMENT x EMPTY> &#37;b;\">\n%b;");
        assert!(
            problem_ids(&dtd).contains(&"entityRecursion"),
            "{:?}",
            dtd.problems
        );
        assert!(dtd.element("x").is_some());
    }

    #[test]
    fn guards_against_exponential_entity_expansion() {
        let mut source = String::from("<!ENTITY lol0 \"lol\">\n");
        for level in 1..=12 {
            let previous = format!("&lol{};", level - 1).repeat(10);
            source.push_str(&format!("<!ENTITY lol{level} \"{previous}\">\n"));
        }
        source.push_str("<!ENTITY a \"&b;\">\n<!ENTITY b \"x&a;\">\n<!ENTITY self \"&self;\">");
        let dtd = parse(&source);
        let lol4 = dtd.general_entity("lol4").unwrap();
        assert_eq!(lol4.expansion.length, 30_000);
        assert_eq!(lol4.expansion.error, None);
        for name in ["lol6", "lol9", "lol12"] {
            assert_eq!(
                dtd.general_entity(name).unwrap().expansion.error,
                Some(ExpansionError::TooLarge),
                "{name}"
            );
        }
        // Le dépassement est signalé une seule fois, sur la première entité.
        let limits = dtd
            .problems
            .iter()
            .filter(|problem| problem.kind == DtdProblemKind::EntityExpansionLimit)
            .collect::<Vec<_>>();
        assert_eq!(limits.len(), 1);
        assert_eq!(text_at(&dtd, &limits[0].location), "lol6");
        for name in ["a", "b", "self"] {
            assert_eq!(
                dtd.general_entity(name).unwrap().expansion.error,
                Some(ExpansionError::Recursive),
                "{name}"
            );
        }
        let recursions = dtd
            .problems
            .iter()
            .filter(|problem| problem.kind == DtdProblemKind::EntityRecursion)
            .map(|problem| text_at(&dtd, &problem.location))
            .collect::<Vec<_>>();
        assert_eq!(recursions, vec!["b", "self"]);

        // Développement d'entités paramètres borné.
        let mut source = String::from(
            "<!ENTITY % l0 \"<!-- xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx -->\">\n",
        );
        for level in 1..=8 {
            let previous = format!("%l{};", level - 1).repeat(10);
            source.push_str(&format!("<!ENTITY % l{level} \"{previous}\">\n"));
        }
        source.push_str("%l8;");
        let dtd = parse(&source);
        assert!(problem_ids(&dtd).contains(&"entityExpansionLimit"));
    }

    #[test]
    fn finds_doctype_declarations() {
        let document = "<?xml version=\"1.0\"?>\n<!-- c -->\n<!DOCTYPE note PUBLIC \"-//ACME//Note\" 'note.dtd' [\n  <!ENTITY a \"]\">\n  <!-- ] -->\n]>\n<note/>";
        let doctype = find_doctype(document).unwrap();
        assert_eq!(doctype.name, "note");
        assert_eq!(&document[doctype.name_range.clone()], "note");
        assert_eq!(doctype.public_id.as_ref().unwrap().0, "-//ACME//Note");
        let (system, range) = doctype.system_id.clone().unwrap();
        assert_eq!(system, "note.dtd");
        assert_eq!(&document[range], "note.dtd");
        let subset = doctype.internal_subset.clone().unwrap();
        assert_eq!(&document[subset], "\n  <!ENTITY a \"]\">\n  <!-- ] -->\n");
        assert!(document[doctype.range.clone()].ends_with("]>"));

        let system = find_doctype("<!DOCTYPE html SYSTEM \"about:legacy-compat\"><html/>").unwrap();
        assert_eq!(system.system_id.unwrap().0, "about:legacy-compat");
        assert_eq!(system.internal_subset, None);
        assert!(find_doctype("<root><!DOCTYPE x></root>").is_none());
        assert!(find_doctype("<root/>").is_none());
    }

    #[test]
    fn loads_internal_and_external_subsets_in_order() {
        let mut loader = MemoryLoader::default();
        loader.files.insert(
            "note.dtd".into(),
            "<!ENTITY % body.content \"(#PCDATA)\">\n<!ELEMENT note (to, body)>\n<!ELEMENT to (#PCDATA)>\n<!ELEMENT body %body.content;>\n<!ENTITY % mod SYSTEM \"module.mod\">\n%mod;\n<!ENTITY sig \"external\">".into(),
        );
        loader
            .files
            .insert("module.mod".into(), "<!ELEMENT extra EMPTY>".into());
        let document = "<!DOCTYPE note SYSTEM \"note.dtd\" [\n  <!ENTITY % body.content \"(#PCDATA | b)*\">\n  <!ENTITY sig \"internal\">\n  <!ELEMENT b (#PCDATA)>\n]>\n<note/>";
        let (doctype, dtd) =
            load_document_dtd(document, Some(PathBuf::from("/docs/note.xml")), &mut loader)
                .unwrap();
        assert!(dtd.problems.is_empty(), "{:?}", dtd.problems);
        assert!(!dtd.incomplete);
        assert_eq!(doctype.name, "note");
        assert_eq!(dtd.doctype_name.as_deref(), Some("note"));
        // Le sous-ensemble interne est prioritaire (première déclaration).
        assert_eq!(
            dtd.element("body").unwrap().content,
            ContentSpec::Mixed(vec!["b".into()])
        );
        assert_eq!(
            dtd.general_entity("sig").unwrap().value,
            EntityValue::Internal("internal".into())
        );
        assert!(dtd.element("extra").is_some());
        let extra = dtd.element("extra").unwrap();
        assert_eq!(
            dtd.source_path(extra.location.source),
            Some(Path::new("/dtd/module.mod"))
        );
        assert_eq!(
            loader.requests[0],
            (
                None,
                "note.dtd".into(),
                Some(PathBuf::from("/docs/note.xml"))
            )
        );
        assert_eq!(loader.requests[1].2, Some(PathBuf::from("/dtd/note.dtd")));
        assert_eq!(dtd.source_path(0), Some(Path::new("/docs/note.xml")));
        // Remontée jusqu'au document : l'élément du module est rattaché à
        // l'identifiant système du DOCTYPE.
        let origin = dtd.origin_in(&extra.location, 0).unwrap();
        assert_eq!(&document[origin.range], "note.dtd");
        assert_eq!(dtd.origin_in(&extra.location, 99), None);
    }

    #[test]
    fn reports_unloadable_external_subsets() {
        let mut loader = MemoryLoader::default();
        let document = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Strict//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd\"><html/>";
        let (_, dtd) = load_document_dtd(document, None, &mut loader).unwrap();
        assert!(dtd.incomplete);
        assert_eq!(
            dtd.problems[0].kind,
            DtdProblemKind::ExternalLoad { remote: true }
        );
        assert_eq!(
            &document[dtd.problems[0].location.range.clone()],
            "http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd"
        );
        assert_eq!(
            loader.requests[0].0.as_deref(),
            Some("-//W3C//DTD XHTML 1.0 Strict//EN")
        );

        // Référence à une entité paramètre inconnue après un échec : pas
        // d'erreur supplémentaire (elle peut venir de la ressource absente).
        let document = "<!DOCTYPE r [ <!ENTITY % ext SYSTEM \"missing.ent\"> %ext; %later; ]><r/>";
        let (_, dtd) = load_document_dtd(document, None, &mut loader).unwrap();
        assert_eq!(problem_ids(&dtd), vec!["externalLoad"]);
    }
}
