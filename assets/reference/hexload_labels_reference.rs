use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct LabelInfo {
    pub pressing_info: String,
    pub is_reissue: bool,
    pub is_audiophile: bool,
    pub country: Option<String>,
    pub label: Option<String>,
}

lazy_static! {
    static ref LABEL_MAPPINGS: HashMap<&'static str, &'static str> = {
        let mut m = HashMap::new();
        
        // Audiophile labels (no country prefix)
        m.insert("MFSL", "MFSL LP  24-96");
        m.insert("MFSL 45", "MFSL 45-RPM Reissue LP  24-96");
        m.insert("MOFI", "MFSL LP  24-96");
        m.insert("DCC", "DCC LP  24-96");
        m.insert("AP", "Analogue Productions Reissue LP  24-96");
        m.insert("AP 33", "Analogue Productions Reissue LP  24-96");
        m.insert("AP 45", "Analogue Productions 45-RPM Reissue LP  24-96");
        m.insert("AP Hoffman", "Analogue Productions SH-Mastered Reissue LP  24-96");
        m.insert("AP SH", "Analogue Productions SH-Mastered Reissue LP  24-96");
        m.insert("Analogue Productions", "Analogue Productions Reissue LP  24-96");
        m.insert("Tone Poet", "Blue Note Tone Poet Reissue LP  24-96");
        m.insert("Music Matters", "Music Matters Jazz Reissue LP  24-96");
        m.insert("Music Matters SRX", "Music Matters SRX 45-RPM Reissue LP  24-96");
        m.insert("MM SRX", "Music Matters SRX 45-RPM Reissue LP  24-96");
        m.insert("MM33", "Music Matters 33-RPM Reissue LP  24-96");
        m.insert("MM 33", "Music Matters 33-RPM Reissue LP  24-96");
        m.insert("Classic Records", "Classic Records Reissue LP  24-96");
        m.insert("Speakers Corner", "Speakers Corner Reissue LP  24-96");
        m.insert("ORG", "ORG Music Reissue LP  24-96");
        m.insert("Impex", "Impex Records Reissue LP  24-96");
        m.insert("Intervention", "Intervention Records Reissue LP  24-96");
        m.insert("Friday Music", "Friday Music Reissue LP  24-96");
        m.insert("FRM", "Friday Music Reissue LP  24-96");
        m.insert("FRMKG", "Friday Music KG-Mastered Reissue LP  24-96");
        m.insert("FRM KG", "Friday Music KG-Mastered Reissue LP  24-96");

        // MFSL/MOFI RPM variants
        m.insert("MFSL 45 RPM", "MFSL 45-RPM Reissue LP  24-96");
        m.insert("MFSL 33 RPM", "MFSL LP  24-96");
        m.insert("MOFI 45", "MFSL 45-RPM Reissue LP  24-96");
        m.insert("MOFI 45 RPM", "MFSL 45-RPM Reissue LP  24-96");

        // AP RPM variants
        m.insert("AP 45 RPM", "Analogue Productions 45-RPM Reissue LP  24-96");
        m.insert("AP 33 RPM", "Analogue Productions Reissue LP  24-96");

        // DCC variants
        m.insert("DCC Compact Classics", "DCC LP  24-96");
        m.insert("DCC LPZ", "DCC LP  24-96");

        // Music Matters RPM variants
        m.insert("MM 33 RPM", "Music Matters 33-RPM Reissue LP  24-96");
        m.insert("MM33 RPM", "Music Matters 33-RPM Reissue LP  24-96");
        m.insert("MM SRX 45", "Music Matters SRX 45-RPM Reissue LP  24-96");
        m.insert("MM SRX 45 RPM", "Music Matters SRX 45-RPM Reissue LP  24-96");
        m.insert("Music Matters 33", "Music Matters 33-RPM Reissue LP  24-96");
        m.insert("Music Matters 33 RPM", "Music Matters 33-RPM Reissue LP  24-96");
        m.insert("Music Matters SRX 45", "Music Matters SRX 45-RPM Reissue LP  24-96");
        m.insert("Music Matters SRX 45 RPM", "Music Matters SRX 45-RPM Reissue LP  24-96");

        // Friday Music full-name variant
        m.insert("Friday Music KG", "Friday Music KG-Mastered Reissue LP  24-96");

        // Short forms / alternate names
        m.insert("Classic", "Classic Records Reissue LP  24-96");
        m.insert("Acoustic Sounds", "Acoustic Sounds Reissue LP  24-96");
        m.insert("MOV", "Music On Vinyl Reissue LP  24-96");
        m.insert("Music On Vinyl", "Music On Vinyl Reissue LP  24-96");
        m.insert("BN Classic", "Blue Note Classic Reissue LP  24-96");
        m.insert("High Roller", "High Roller Records Reissue LP  24-96");

        // UK variations
        m.insert("UK", "UK First-Press LP  24-96");
        m.insert("UK RL", "UK RL-Mastered LP  24-96");
        m.insert("UK Porky", "UK Porky-Mastered LP  24-96");
        m.insert("UK Pecko", "UK Porky-Mastered LP  24-96");  // Another Peckham variant
        m.insert("UK PorkyArun", "UK Porky/Arun-Mastered LP  24-96");
        m.insert("PorkyArun", "UK Porky/Arun-Mastered LP  24-96");
        m.insert("UK Miles", "UK Miles-Mastered LP  24-96");  // Miles Showell
        m.insert("UK Townhouse", "UK Townhouse LP  24-96");
        m.insert("Tube Cut", "UK Tube-Cut LP  24-96");
        m.insert("Tube cut", "UK Tube-Cut LP  24-96");
        m.insert("Tube-cut", "UK Tube-Cut LP  24-96");
        m.insert("tube-cut", "UK Tube-Cut LP  24-96");
        m.insert("tube cut", "UK Tube-Cut LP  24-96");
        m.insert("UK Tube Cut", "UK Tube-Cut LP  24-96");
        m.insert("UK Tube-Cut", "UK Tube-Cut LP  24-96");
        m.insert("UK Townhouse Tube-Cut", "UK Townhouse Tube-Cut LP  24-96");
        m.insert("UK Abbey Road Tube-Cut", "UK Abbey Road Tube-Cut LP  24-96");
        m.insert("UK Abbey Road", "UK Abbey Road LP  24-96");
        m.insert("UK HTM", "UK HTM-Mastered LP  24-96");
        m.insert("HTM", "UK HTM-Mastered LP  24-96");  // HTM usually indicates UK pressing
        m.insert("UK Bilbo", "UK Bilbo-Mastered LP  24-96");  // Denis Blackham
        m.insert("UK Nimbus", "UK First-Press LP  24-96");
        m.insert("UK Mono", "UK Mono First-Press LP  24-96");
        m.insert("UK Mono EP", "UK Mono First-Press EP  24-96");
        m.insert("UK Plum", "UK Plum First-Press LP  24-96");
        m.insert("Plum", "UK Plum First-Press LP  24-96");
        m.insert("UK Track", "UK Track First-Press LP  24-96");
        m.insert("UK Vertigo", "UK Vertigo First-Press LP  24-96");
        m.insert("UK Island", "UK Island First-Press LP  24-96");
        m.insert("UK Harvest", "UK Harvest First-Press LP  24-96");
        m.insert("UK EMI", "UK EMI First-Press LP  24-96");
        m.insert("UK Decca", "UK Decca First-Press LP  24-96");
        m.insert("UK Columbia", "UK Columbia First-Press LP  24-96");
        m.insert("UK Parlophone", "UK Parlophone First-Press LP  24-96");
        m.insert("UK Apple", "UK Apple First-Press LP  24-96");
        m.insert("UK Atlantic", "UK Atlantic First-Press LP  24-96");
        m.insert("UK Factory", "UK Factory First-Press LP  24-96");
        m.insert("UK 4AD", "UK 4AD First-Press LP  24-96");
        m.insert("UK Creation", "UK Creation First-Press LP  24-96");
        m.insert("UK Rough Trade", "UK Rough Trade First-Press LP  24-96");
        m.insert("UK Promo", "UK Promo LP  24-96");
        m.insert("UK Test Pressing", "UK Test Pressing LP  24-96");
        
        // US variations
        m.insert("US", "US First-Press LP  24-96");
        m.insert("US RL", "US RL-Mastered LP  24-96");
        m.insert("US Sterling", "US Sterling-Mastered LP  24-96");
        m.insert("US Sterling RL", "US Sterling RL-Mastered LP  24-96");
        m.insert("US Masterdisk RL", "US RL-Mastered LP  24-96");
        m.insert("US KG", "US Kevin Gray Mastered LP  24-96");
        m.insert("US BG", "US Bernie Grundman Mastered LP  24-96");
        m.insert("US CB", "US Chris Bellman Mastered LP  24-96");
        m.insert("US WLP", "US White-Label Promo LP  24-96");
        m.insert("WLP", "US White-Label Promo LP  24-96");
        m.insert("WLP Mono", "US White-Label Promo Mono LP  24-96");
        m.insert("US Promo", "US Promo LP  24-96");
        m.insert("Promo", "US Promo LP  24-96");  // Default to US if country not specified
        m.insert("US Test Pressing", "US Test Pressing LP  24-96");
        m.insert("RL", "US RL-Mastered First-Press LP  24-96");
        m.insert("Piros", "US Piros-Mastered First-Press LP  24-96");
        m.insert("Piros Monarch", "US Piros-Mastered Monarch Press LP  24-96");
        m.insert("AT/GP", "US Piros-Mastered First-Press LP  24-96");  // Atlantic/George Piros
        m.insert("AT GP", "US Piros-Mastered First-Press LP  24-96");
        m.insert("GP", "US George Piros Mastered LP  24-96");
        m.insert("George Piros", "US George Piros Mastered LP  24-96");
        m.insert("CSM", "US CSM-Mastered LP  24-96");
        m.insert("CSM RL", "US CSM/RL-Mastered LP  24-96");
        m.insert("CSM Wally", "US CSM/Wally-Mastered LP  24-96");
        m.insert("TML", "US TML Press LP  24-96");
        m.insert("TML-M", "US TML-M Press LP  24-96");
        m.insert("Gilbert Kong", "US Gilbert Kong Mastered LP  24-96");
        m.insert("Sterling", "US Sterling-Mastered LP  24-96");
        m.insert("Masterdisk", "US Masterdisk LP  24-96");
        m.insert("Masterdisk RL", "US RL-Mastered LP  24-96");
        m.insert("Bernie Grundman", "US Bernie Grundman-Mastered LP  24-96");
        m.insert("BG", "US Bernie Grundman-Mastered LP  24-96");
        m.insert("Kevin Gray", "US Kevin Gray Mastered LP  24-96");
        m.insert("KG", "US Kevin Gray Mastered LP  24-96");
        m.insert("Steve Hoffman", "US Steve Hoffman Mastered LP  24-96");
        m.insert("SH", "US Steve Hoffman Mastered LP  24-96");
        m.insert("Chris Bellman", "US Chris Bellman Mastered LP  24-96");
        m.insert("CB", "US Chris Bellman Mastered LP  24-96");
        m.insert("Ryan Smith", "US Ryan Smith Mastered LP  24-96");
        m.insert("RS", "US Ryan Smith Mastered LP  24-96");
        m.insert("Bob Weston", "US Bob Weston Mastered LP  24-96");
        m.insert("Wally", "US Wally-Mastered LP  24-96");
        m.insert("Wally Traugott", "US Wally-Mastered LP  24-96");
        m.insert("WT", "US Wally-Mastered LP  24-96");
        m.insert("Stan Ricker", "US Stan Ricker Mastered LP  24-96");
        m.insert("SR", "US Stan Ricker Mastered LP  24-96");
        m.insert("Doug Sax", "US Doug Sax Mastered LP  24-96");
        m.insert("DS", "US Doug Sax Mastered LP  24-96");
        m.insert("Terre Haute", "US Terre Haute Press LP  24-96");
        m.insert("Kendun", "US Kendun LP  24-96");  // Kent Duncan mastering
        m.insert("K-Disc", "US K-Disc Mastered LP  24-96");  // K-Disc mastering (John Golden)
        m.insert("Presswell", "US Presswell Press LP  24-96");
        m.insert("PR", "US Presswell Press LP  24-96");  // Presswell code
        m.insert("PRC", "US PRC (Presswell) Press LP  24-96");
        m.insert("PRC Compton", "US PRC Compton Press LP  24-96");
        m.insert("PRC Richmond", "US PRC Richmond Press LP  24-96");
        m.insert("Pogo", "Pogo Pressing LP  24-96");  // Keith Olson mastering engineer
        m.insert("Allied", "US Allied Press LP  24-96");  // Allied Record Corp, Los Angeles
        m.insert("Capitol Winchester", "US Capitol Winchester Press LP  24-96");  // Capitol Records Winchester, VA
        m.insert("Winchester", "US Capitol Winchester Press LP  24-96");  // Capitol Winchester symbol
        m.insert("Gloversville", "US MCA Gloversville Press LP  24-96");  // MCA/ex-Decca Gloversville, NY
        m.insert("Pickneyville", "US MCA Pickneyville Press LP  24-96");  // MCA/ex-Decca Pickneyville
        m.insert("Damont", "UK Damont Press LP  24-96");  // Damont Audio Limited (UK)
        m.insert("Eurodisc", "US Eurodisc Press LP  24-96");  // Eurodisc NYC
        m.insert("Europadisk", "US Europadisk Press LP  24-96");  // Europadisk NYC
        m.insert("MPO", "FR MPO Press LP  24-96");  // Moulages et Plastiques de l'Ouest, France
        m.insert("Orlake", "UK Orlake Press LP  24-96");  // Orlake Records, Dagenham UK
        m.insert("RTI", "US RTI Press LP  24-96");  // Record Technology Inc.
        m.insert("QRP", "US QRP Press LP  24-96");  // Quality Record Pressings
        m.insert("Pallas", "German Pallas Press LP  24-96");  // Pallas GmbH, Germany
        m.insert("Optimal", "German Optimal Press LP  24-96");  // Optimal Media, Germany
        m.insert("Specialty", "US Specialty Records Press LP  24-96");
        m.insert("Monarch", "US Monarch Press LP  24-96");
        m.insert("MO", "US Monarch Press LP  24-96");
        m.insert("Monarch RL", "US RL-Mastered Monarch Press LP  24-96");
        m.insert("RL Monarch", "US RL-Mastered Monarch Press LP  24-96");
        m.insert("MO RL", "US RL-Mastered Monarch Press LP  24-96");
        m.insert("US Monarch RL", "US RL-Mastered Monarch Press LP  24-96");
        m.insert("Minimax", "Minimax LP  24-96");
        m.insert("US Minimax", "US Minimax First-Press LP  24-96");
        m.insert("UK Minimax", "UK Minimax First-Press LP  24-96");
        m.insert("Quiex", "US Quiex II Pressing LP  24-96");
        
        // Japanese variations
        m.insert("Japan", "Japan First-Press LP  24-96");
        m.insert("Japan Mono", "Japan Mono First-Press LP  24-96");
        m.insert("Japan Warner Pioneer", "Japan Warner Pioneer First-Press LP  24-96");
        m.insert("Japan Epic Sony", "Japan Epic/Sony First-Press LP  24-96");
        m.insert("Japan Atlantic", "Japan Atlantic First-Press LP  24-96");
        m.insert("Japan CBS-Sony", "Japan CBS-Sony First-Press LP  24-96");
        m.insert("Japan King", "Japan King Records First-Press LP  24-96");
        m.insert("Japan King Records", "Japan King Records First-Press LP  24-96");
        m.insert("Japan Odeon", "Japan Odeon First-Press LP  24-96");
        m.insert("Japan Charisma", "Japan Charisma First-Press LP  24-96");
        m.insert("Japan Toshiba Pro-Use", "Japan Toshiba Pro-Use Series LP  24-96");
        m.insert("Toshiba Japan Pro-Use Series", "Japan Toshiba Pro-Use Series LP  24-96");
        m.insert("Japan Pro-Use", "Japan Toshiba Pro-Use Series LP  24-96");
        m.insert("Japan Mastersound", "Japan CBS-Sony Mastersound LP  24-96");
        m.insert("Japan CBS-Sony Mastersound", "Japan CBS-Sony Mastersound LP  24-96");
        m.insert("Japan Harvest", "Japan Harvest First-Press LP  24-96");
        m.insert("Japan Polydor", "Japan Polydor First-Press LP  24-96");
        m.insert("Japan Vertigo", "Japan Vertigo First-Press LP  24-96");
        m.insert("Japan Denon", "Japan Denon First-Press LP  24-96");
        m.insert("Japan A&M", "Japan A&M First-Press LP  24-96");
        m.insert("Japan Elektra", "Japan Elektra First-Press LP  24-96");
        m.insert("Japan London", "Japan London Records First-Press LP  24-96");
        m.insert("Japan Philips", "Japan Philips First-Press LP  24-96");
        m.insert("Red Wax", "Japan Red Wax First-Press LP  24-96");
        m.insert("Red Wax Mono", "Japan Red Wax Mono First-Press LP  24-96");
        
        // German/West German
        m.insert("German", "German First-Press LP  24-96");  // Default to "German", will be adjusted based on year
        m.insert("West German", "West German First-Press LP  24-96");
        m.insert("German Odeon", "West German Odeon First-Press LP  24-96");
        m.insert("West German Odeon", "West German Odeon First-Press LP  24-96");
        m.insert("Teldec", "West German Teldec First-Press LP  24-96");
        m.insert("Horzu", "West German Horzu First-Press LP  24-96");
        m.insert("Hörzu", "West German Hörzu First-Press LP  24-96");
        m.insert("DE", "DE First-Press LP  24-96");
        
        // Other countries
        m.insert("Canada", "CA First-Press LP  24-96");
        m.insert("Canadian", "CA First-Press LP  24-96");
        m.insert("CAN", "CA First-Press LP  24-96");
        m.insert("CA", "CA First-Press LP  24-96");
        m.insert("Canada Mono", "CA Mono First-Press LP  24-96");
        m.insert("Australia", "AUS First-Press LP  24-96");
        m.insert("Australian", "AUS First-Press LP  24-96");
        m.insert("AUS", "AUS First-Press LP  24-96");
        m.insert("New Zealand", "NZ First-Press LP  24-96");
        m.insert("NZ", "NZ First-Press LP  24-96");
        m.insert("Norway", "Norway First-Press LP  24-96");
        m.insert("Norwegian", "Norway First-Press LP  24-96");
        m.insert("Austria", "AUT First-Press LP  24-96");
        m.insert("Austrian", "AUT First-Press LP  24-96");
        m.insert("AUT", "AUT First-Press LP  24-96");
        m.insert("Switzerland", "Swiss First-Press LP  24-96");
        m.insert("Swiss", "Swiss First-Press LP  24-96");
        m.insert("CH", "Swiss First-Press LP  24-96");
        m.insert("France", "FR First-Press LP  24-96");
        m.insert("French", "FR First-Press LP  24-96");
        m.insert("FRA", "FR First-Press LP  24-96");
        m.insert("FR", "FR First-Press LP  24-96");
        m.insert("Italy", "IT First-Press LP  24-96");
        m.insert("Italian", "IT First-Press LP  24-96");
        m.insert("ITA", "IT First-Press LP  24-96");
        m.insert("IT", "IT First-Press LP  24-96");
        m.insert("Spain", "ESP First-Press LP  24-96");
        m.insert("Spanish", "ESP First-Press LP  24-96");
        m.insert("ESP", "ESP First-Press LP  24-96");
        m.insert("ES", "ESP First-Press LP  24-96");
        m.insert("Netherlands", "NL First-Press LP  24-96");
        m.insert("Dutch", "NL First-Press LP  24-96");
        m.insert("Holland", "NL First-Press LP  24-96");
        m.insert("NL", "NL First-Press LP  24-96");
        m.insert("Belgium", "Belgium First-Press LP  24-96");
        m.insert("Belgian", "Belgium First-Press LP  24-96");
        m.insert("BE", "Belgium First-Press LP  24-96");
        m.insert("South Africa", "South Africa First-Press LP  24-96");
        m.insert("South African", "South Africa First-Press LP  24-96");
        m.insert("RSA", "South Africa First-Press LP  24-96");
        m.insert("Sweden", "Sweden First-Press LP  24-96");
        m.insert("Swedish", "Sweden First-Press LP  24-96");
        m.insert("SE", "Sweden First-Press LP  24-96");
        m.insert("Finland", "Finland First-Press LP  24-96");
        m.insert("Finnish", "Finland First-Press LP  24-96");
        m.insert("FI", "Finland First-Press LP  24-96");
        m.insert("EU", "EU Press LP  24-96");
        
        // Reissues (various)
        m.insert("2022 Reissue", "2022 Reissue LP  24-96");
        m.insert("2023 Reissue", "2023 Reissue LP  24-96");
        m.insert("2024 Reissue", "2024 Reissue LP  24-96");
        m.insert("2025 Reissue", "2025 Reissue LP  24-96");
        m.insert("2019 VMP Reissue", "VMP Reissue LP  24-96");
        m.insert("2017 Reissue", "2017 Reissue LP  24-96");
        m.insert("2018 Japan Reissue", "Japan 2018 Reissue LP  24-96");
        m.insert("2019 Japan Reissue", "Japan 2019 Reissue LP  24-96");
        m.insert("2025 Japan Reissue", "Japan 2025 Reissue LP  24-96");
        
        // German/DE reissues (NEVER West German)
        m.insert("German 2022 Reissue", "German 2022 Reissue LP  24-96");
        m.insert("German 2023 Reissue", "German 2023 Reissue LP  24-96");
        m.insert("German 2024 Reissue", "German 2024 Reissue LP  24-96");
        m.insert("German 2025 Reissue", "German 2025 Reissue LP  24-96");
        m.insert("German Reissue", "German Reissue LP  24-96");
        m.insert("German Repress", "German Reissue LP  24-96");
        m.insert("2020 German Repress", "German 2020 Reissue LP  24-96");
        m.insert("DE Reissue", "DE Reissue LP  24-96");
        m.insert("DE Repress", "DE Reissue LP  24-96");
        m.insert("DE 2022 Reissue", "DE 2022 Reissue LP  24-96");
        m.insert("DE 2023 Reissue", "DE 2023 Reissue LP  24-96");
        m.insert("DE 2024 Reissue", "DE 2024 Reissue LP  24-96");
        m.insert("DE 2025 Reissue", "DE 2025 Reissue LP  24-96");
        
        // UK reissues
        m.insert("UK 2022 Reissue", "UK 2022 Reissue LP  24-96");
        m.insert("UK 2023 Reissue", "UK 2023 Reissue LP  24-96");
        m.insert("UK Reissue", "UK Reissue LP  24-96");
        m.insert("UK Repress", "UK Reissue LP  24-96");
        m.insert("UHQR", "UHQR Reissue LP  24-96");
        m.insert("VMP", "VMP Reissue LP  24-96");
        m.insert("Atlantic 75", "Atlantic 75 Reissue LP  24-96");
        m.insert("Rhino", "Rhino Reissue LP  24-96");
        m.insert("2025 Rhino", "Rhino 2025 Reissue LP  24-96");
        m.insert("2025 45 RPM Reissue", "45-RPM Reissue LP  24-96");
        m.insert("Craft Reissue", "Craft Reissue LP  24-96");
        m.insert("Studio One Reissue", "Studio One Reissue LP  24-96");
        m.insert("Matador Reissue", "Matador Reissue LP  24-96");
        m.insert("45 RPM", "45-RPM LP  24-96");
        
        // Special formats - using lowercase "7-inch" and "12-inch" without "Single"
        m.insert("7 Inch", "US 7-inch  24-96");
        m.insert("7 Inch UK", "UK 7-inch  24-96");
        m.insert("7 Inch UK EP", "UK 7-inch EP  24-96");
        m.insert("7 Inch Canada", "CA 7-inch  24-96");
        m.insert("7 Inch Australia RSD", "AUS RSD 7-inch  24-96");
        m.insert("7 Inch New Zealand", "NZ 7-inch  24-96");
        m.insert("7 Inch Mono EP", "US 7-inch Mono EP  24-96");
        m.insert("7 Inch EP", "US 7-inch EP  24-96");
        m.insert("7 Inch UK Ep", "UK 7-inch EP  24-96");  // Handle case variations
        m.insert("7 Inch Mono Motown Yesteryear", "Motown Yesteryear 7-inch Mono  24-96");
        m.insert("Wipers - Alien Boy (7 Inch EP)", "US 7-inch EP  24-96");  // This won't match the full name
        m.insert("12 Inch", "US 12-inch  24-96");
        m.insert("12 Inch EP", "US 12-inch EP  24-96");
        m.insert("12 Inch UK", "UK 12-inch  24-96");
        m.insert("12 Inch UK EP", "UK 12-inch EP  24-96");
        m.insert("12 Inch 1985 German", "German 1985 12-inch  24-96");  // Will be adjusted to West German by year logic
        m.insert("12 Inch Belgium", "Belgium 12-inch  24-96");
        m.insert("Mono", "Mono First-Press LP  24-96");
        m.insert("UK Mono", "UK Mono First-Press LP  24-96");
        m.insert("German Decca Mono", "West German Decca Mono First-Press LP  24-96");
        m.insert("BN75", "Blue Note 75 Reissue LP  24-96");
        m.insert("BN 75", "Blue Note 75 Reissue LP  24-96");
        m.insert("Blue Note 75", "Blue Note 75 Reissue LP  24-96");
        m.insert("BN80", "Blue Note 80 Reissue LP  24-96");
        m.insert("BN 80", "Blue Note 80 Reissue LP  24-96");
        m.insert("Blue Note 80", "Blue Note 80 Reissue LP  24-96");
        m.insert("Hi-Q", "Hi-Q Records Reissue LP  24-96");
        m.insert("Hi-Q Records", "Hi-Q Records Reissue LP  24-96");
        m.insert("HiQ", "Hi-Q Records Reissue LP  24-96");
        m.insert("Box Set", "Box Set LP  24-96");
        m.insert("2012 Box Set", "2012 Box Set LP  24-96");
        m.insert("ACDC Box Set Volume 2", "Box Set Volume 2 LP  24-96");
        
        m
    };
    
    static ref AUDIOPHILE_LABELS: Vec<&'static str> = vec![
        "MFSL", "MOFI", "DCC", "AP", "Analogue Productions", "Tone Poet",
        "Music Matters", "MM", "Classic Records", "Classic", "Speakers Corner",
        "ORG", "Impex", "Intervention", "Friday Music", "FRM", "UHQR",
        "Hi-Q", "HiQ", "Acoustic Sounds", "MOV", "Music On Vinyl",
        "Blue Note 75", "Blue Note 80", "BN Classic", "BN"
    ];
    
    static ref REISSUE_KEYWORDS: Vec<&'static str> = vec![
        "Reissue", "Repress", "Remastered", "Anniversary", "Edition",
        "Box Set", "45 RPM", "UHQR", "VMP", "Rhino", "Atlantic 75"
    ];

    /// Audiophile label prefixes sorted longest-first for prefix matching.
    /// Each tuple: (prefix, base LABEL_MAPPINGS key to fall back to).
    static ref AUDIOPHILE_PREFIX_FALLBACKS: Vec<(&'static str, &'static str)> = vec![
        ("Analogue Productions", "Analogue Productions"),
        ("Acoustic Sounds", "Acoustic Sounds"),
        ("Music On Vinyl", "MOV"),
        ("Music Matters", "Music Matters"),
        ("Classic Records", "Classic Records"),
        ("Speakers Corner", "Speakers Corner"),
        ("Friday Music", "Friday Music"),
        ("Intervention", "Intervention"),
        ("Tone Poet", "Tone Poet"),
        ("Hi-Q Records", "Hi-Q"),
        ("Hi-Q", "Hi-Q"),
        ("HiQ", "Hi-Q"),
        ("Impex", "Impex"),
        ("UHQR", "UHQR"),
        ("MFSL", "MFSL"),
        ("MOFI", "MFSL"),
        ("DCC", "DCC"),
        ("FRM", "FRM"),
        ("ORG", "ORG"),
        ("MOV", "MOV"),
        ("AP", "AP"),
        ("MM", "Music Matters"),
        ("BN", "BN Classic"),
    ];
}

pub fn detect_pressing_info(folder_name: &str, year: Option<&str>) -> LabelInfo {
    let mut info = LabelInfo {
        pressing_info: "US First-Press LP  24-96".to_string(),
        is_reissue: false,
        is_audiophile: false,
        country: None,
        label: None,
    };
    
    // Check for reissue keywords
    for keyword in REISSUE_KEYWORDS.iter() {
        if folder_name.contains(keyword) {
            info.is_reissue = true;
            break;
        }
    }
    
    // Try to extract pressing info from parentheses
    let re = Regex::new(r"\(([^)]+)\)").unwrap();
    if let Some(caps) = re.captures(folder_name) {
        let content = caps.get(1).unwrap().as_str();
        
        // First check for exact match
        if let Some(mapping) = LABEL_MAPPINGS.get(content) {
            info.pressing_info = mapping.to_string();
            
            // Check if it's audiophile
            for label in AUDIOPHILE_LABELS.iter() {
                if content.contains(label) {
                    info.is_audiophile = true;
                    break;
                }
            }
            
            // Handle German/West German based on year
            if content.contains("German") && !content.contains("West German") {
                if let Some(year_str) = year {
                    if let Ok(year_num) = year_str.parse::<i32>() {
                        if year_num < 1990 && !info.is_reissue {
                            // Pre-1990 German should be West German (unless it's a reissue)
                            // But only if "West German" isn't already in the pressing_info
                            if !info.pressing_info.contains("West German") {
                                info.pressing_info = info.pressing_info.replace("German", "West German");
                            }
                        }
                    }
                }
            }
            
            // Extract country
            if info.pressing_info.starts_with("UK ") {
                info.country = Some("UK".to_string());
            } else if info.pressing_info.starts_with("US ") {
                info.country = Some("US".to_string());
            } else if info.pressing_info.starts_with("Japan ") {
                info.country = Some("Japan".to_string());
            } else if info.pressing_info.starts_with("West German ") {
                info.country = Some("West German".to_string());
            } else if info.pressing_info.starts_with("DE ") || info.pressing_info.starts_with("German ") {
                info.country = Some("DE".to_string());
            } else if info.pressing_info.starts_with("Canada ") {
                info.country = Some("Canada".to_string());
            } else if info.pressing_info.starts_with("FR ") {
                info.country = Some("France".to_string());
            }
            
            return info;
        }

        // Check for 7-inch/12-inch format with smart parsing
        if let Some(parsed_info) = parse_inch_format(content) {
            info.pressing_info = parsed_info.clone();

            // Extract country for info struct
            if parsed_info.starts_with("UK ") {
                info.country = Some("UK".to_string());
            } else if parsed_info.starts_with("US ") {
                info.country = Some("US".to_string());
            } else if parsed_info.starts_with("Japan ") {
                info.country = Some("Japan".to_string());
            } else if parsed_info.starts_with("West German ") {
                info.country = Some("West German".to_string());
            } else if parsed_info.starts_with("German ") {
                info.country = Some("German".to_string());
            } else if parsed_info.starts_with("CA ") {
                info.country = Some("CA".to_string());
            } else if parsed_info.starts_with("AUS ") {
                info.country = Some("AUS".to_string());
            } else if parsed_info.starts_with("NZ ") {
                info.country = Some("NZ".to_string());
            } else if parsed_info.starts_with("FR ") {
                info.country = Some("FR".to_string());
            } else if parsed_info.starts_with("ESP ") {
                info.country = Some("ESP".to_string());
            } else if parsed_info.starts_with("IT ") {
                info.country = Some("IT".to_string());
            } else if parsed_info.starts_with("NL ") {
                info.country = Some("NL".to_string());
            } else if parsed_info.starts_with("Belgium ") {
                info.country = Some("Belgium".to_string());
            } else if parsed_info.starts_with("South Africa ") {
                info.country = Some("South Africa".to_string());
            } else if parsed_info.starts_with("Norway ") {
                info.country = Some("Norway".to_string());
            } else if parsed_info.starts_with("Sweden ") {
                info.country = Some("Sweden".to_string());
            } else if parsed_info.starts_with("Finland ") {
                info.country = Some("Finland".to_string());
            } else if parsed_info.starts_with("AUT ") {
                info.country = Some("AUT".to_string());
            } else if parsed_info.starts_with("Swiss ") {
                info.country = Some("Swiss".to_string());
            }

            return info;
        }

        // Audiophile label prefix fallback: catch unknown variants like
        // "MFSL 45 RPM" or "DCC Gold" that have no exact LABEL_MAPPINGS key.
        for (prefix, base_key) in AUDIOPHILE_PREFIX_FALLBACKS.iter() {
            // Boundary check: prefix must be followed by a space or be the entire string.
            // Prevents "AP" matching "Apple" or "MM" matching "Mono Motown".
            if content == *prefix
                || (content.starts_with(prefix)
                    && content.as_bytes().get(prefix.len()) == Some(&b' '))
            {
                if let Some(base_mapping) = LABEL_MAPPINGS.get(base_key) {
                    log::warn!(
                        "Audiophile label prefix fallback: '{}' matched prefix '{}', \
                         using base mapping '{}'. Consider adding an exact LABEL_MAPPINGS entry.",
                        content, prefix, base_mapping
                    );
                    info.pressing_info = base_mapping.to_string();
                    info.is_audiophile = true;
                    info.is_reissue = true;
                    return info;
                }
            }
        }

        // Dynamic pattern: <YEAR> Reissue (any year not covered by static entries)
        let reissue_re = Regex::new(r"^(\d{4})\s+Reissue$").unwrap();
        if let Some(caps) = reissue_re.captures(content) {
            let yr = caps.get(1).unwrap().as_str();
            info.pressing_info = format!("{} Reissue LP  24-96", yr);
            info.is_reissue = true;
            return info;
        }

        // Dynamic pattern: <Country> <YEAR> Reissue (any year not covered by static entries)
        let country_reissue_re = Regex::new(r"^(\w[\w\s]*?)\s+(\d{4})\s+Reissue$").unwrap();
        if let Some(caps) = country_reissue_re.captures(content) {
            let country = caps.get(1).unwrap().as_str();
            let yr = caps.get(2).unwrap().as_str();
            info.pressing_info = format!("{} {} Reissue LP  24-96", country, yr);
            info.is_reissue = true;
            return info;
        }

        // Dynamic pattern: <Country> Minimax (any country not covered by static entries)
        let minimax_re = Regex::new(r"^(\w[\w\s]*?)\s+Minimax$").unwrap();
        if let Some(caps) = minimax_re.captures(content) {
            let country = caps.get(1).unwrap().as_str();
            info.pressing_info = format!("{} Minimax First-Press LP  24-96", country);
            return info;
        }

        // Dynamic pattern: colored vinyl — <Country/Year> <Color> Vinyl
        // Colored vinyl pressings are reissues
        // Use separate patterns for year-prefixed vs country-prefixed to avoid ambiguity
        let colors = "White|Red|Blue|Green|Gold|Clear|Orange|Yellow|Pink|Purple|Black|Silver|Grey|Gray|Marble|Splatter|Translucent|Colored|Coloured";

        // Pattern: <Year> <Color> Vinyl  or  <Country> <Year> <Color> Vinyl
        let year_vinyl_re = Regex::new(
            &format!(r"(?i)^(?:([A-Za-z][\w\s]*?)\s+)?(\d{{4}})\s+((?:{})\s+Vinyl)$", colors)
        ).unwrap();
        if let Some(caps) = year_vinyl_re.captures(content) {
            let country_part = caps.get(1).map(|m| m.as_str());
            let year_part = caps.get(2).unwrap().as_str();
            let vinyl_part = caps.get(3).unwrap().as_str();
            let vinyl_normalized = normalize_vinyl_case(vinyl_part);
            let prefix = match country_part {
                Some(c) => format!("{} {} ", c, year_part),
                None => format!("US {} ", year_part),
            };
            info.pressing_info = format!("{}{} Reissue LP  24-96", prefix, vinyl_normalized);
            info.is_reissue = true;
            return info;
        }

        // Pattern: <Country> <Color> Vinyl  or  bare <Color> Vinyl
        let vinyl_re = Regex::new(
            &format!(r"(?i)^(?:([A-Za-z][\w\s]*?)\s+)?((?:{})\s+Vinyl)$", colors)
        ).unwrap();
        if let Some(caps) = vinyl_re.captures(content) {
            let country_part = caps.get(1).map(|m| m.as_str());
            let vinyl_part = caps.get(2).unwrap().as_str();
            let vinyl_normalized = normalize_vinyl_case(vinyl_part);
            let prefix = match country_part {
                Some(c) => format!("{} ", c),
                None => "US ".to_string(),
            };
            info.pressing_info = format!("{}{} Reissue LP  24-96", prefix, vinyl_normalized);
            info.is_reissue = true;
            return info;
        }

        // No exact match, check if content starts with a country identifier
        // This handles cases like "(Spanish CBS-123)" or "(French Polydor)"
        let country_prefixes = vec![
            ("Spanish", "ESP First-Press LP  24-96"),
            ("Spain", "ESP First-Press LP  24-96"),
            ("French", "FR First-Press LP  24-96"),
            ("France", "FR First-Press LP  24-96"),
            ("Italian", "IT First-Press LP  24-96"),
            ("Italy", "IT First-Press LP  24-96"),
            ("German", "German First-Press LP  24-96"),
            ("West German", "West German First-Press LP  24-96"),
            ("Dutch", "NL First-Press LP  24-96"),
            ("Netherlands", "NL First-Press LP  24-96"),
            ("Holland", "NL First-Press LP  24-96"),
            ("Belgian", "Belgium First-Press LP  24-96"),
            ("Belgium", "Belgium First-Press LP  24-96"),
            ("South Africa", "South Africa First-Press LP  24-96"),
            ("South African", "South Africa First-Press LP  24-96"),
            ("Austrian", "AUT First-Press LP  24-96"),
            ("Austria", "AUT First-Press LP  24-96"),
            ("Swiss", "Swiss First-Press LP  24-96"),
            ("Switzerland", "Swiss First-Press LP  24-96"),
            ("Canadian", "CA First-Press LP  24-96"),
            ("Canada", "CA First-Press LP  24-96"),
            ("Australian", "AUS First-Press LP  24-96"),
            ("Australia", "AUS First-Press LP  24-96"),
            ("New Zealand", "NZ First-Press LP  24-96"),
            ("NZ", "NZ First-Press LP  24-96"),
            ("Norwegian", "Norway First-Press LP  24-96"),
            ("Norway", "Norway First-Press LP  24-96"),
            ("Swedish", "Sweden First-Press LP  24-96"),
            ("Sweden", "Sweden First-Press LP  24-96"),
            ("Finnish", "Finland First-Press LP  24-96"),
            ("Finland", "Finland First-Press LP  24-96"),
            ("UK", "UK First-Press LP  24-96"),
            ("US", "US First-Press LP  24-96"),
            ("Japan", "Japan First-Press LP  24-96"),
            ("Japanese", "Japan First-Press LP  24-96"),
        ];
        
        for (prefix, pressing) in country_prefixes {
            if content.starts_with(prefix) {
                info.pressing_info = pressing.to_string();
                
                // Handle German/West German based on year
                if prefix == "German" && !content.contains("West German") {
                    if let Some(year_str) = year {
                        if let Ok(year_num) = year_str.parse::<i32>() {
                            if year_num < 1990 && !info.is_reissue {
                                info.pressing_info = info.pressing_info.replace("German", "West German");
                            }
                        }
                    }
                }
                
                return info;
            }
        }
    }
    
    // Try to extract from square brackets [source]
    let re = Regex::new(r"\[([^\]]+)\]").unwrap();
    if let Some(caps) = re.captures(folder_name) {
        let content = caps.get(1).unwrap().as_str();
        
        if let Some(mapping) = LABEL_MAPPINGS.get(content) {
            info.pressing_info = mapping.to_string();
        }
    }
    
    info
}

/// Normalize vinyl color case: "blue vinyl" → "Blue Vinyl", "WHITE VINYL" → "White Vinyl"
fn normalize_vinyl_case(s: &str) -> String {
    s.split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().to_string() + &chars.as_str().to_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse 7-inch or 12-inch format with smart country and qualifier detection
/// Returns formatted string like "Japan 7-inch  24-96" or "AUS RSD 7-inch Mono  24-96"
fn parse_inch_format(content: &str) -> Option<String> {
    let content_lower = content.to_lowercase();

    // Check if this is a 7-inch or 12-inch format
    let format = if content_lower.contains("7 inch") {
        "7-inch"
    } else if content_lower.contains("12 inch") {
        "12-inch"
    } else {
        return None;
    };

    // Detect country (check longer strings first to avoid false matches)
    let country = if content.contains("New Zealand") {
        "NZ"
    } else if content.contains("South Africa") || content.contains("South African") {
        "South Africa"
    } else if content.contains("West German") {
        "West German"
    } else if content.contains("Australia") || content.contains("Australian") {
        "AUS"
    } else if content.contains("Netherlands") || content.contains("Dutch") || content.contains("Holland") {
        "NL"
    } else if content.contains("Switzerland") || content.contains("Swiss") {
        "Swiss"
    } else if content.contains("Belgium") || content.contains("Belgian") {
        "Belgium"
    } else if content.contains("Austria") || content.contains("Austrian") {
        "AUT"
    } else if content.contains("Norway") || content.contains("Norwegian") {
        "Norway"
    } else if content.contains("Sweden") || content.contains("Swedish") || content.contains("Finland") || content.contains("Finnish") {
        // Scandinavian countries without dedicated ISO codes in this system
        if content.contains("Finland") || content.contains("Finnish") {
            "Finland"
        } else {
            "Sweden"
        }
    } else if content.contains("Canada") || content.contains("Canadian") {
        "CA"
    } else if content.contains("Japan") || content.contains("Japanese") {
        "Japan"
    } else if content.contains("Spain") || content.contains("Spanish") {
        "ESP"
    } else if content.contains("France") || content.contains("French") {
        "FR"
    } else if content.contains("Italy") || content.contains("Italian") {
        "IT"
    } else if content.contains("German") {
        "German"
    } else if content.contains("UK") {
        "UK"
    } else if content.contains("US") {
        "US"
    } else {
        "US"  // Default
    };

    // Detect pre-format qualifiers (go before format)
    let mut pre_qualifiers = Vec::new();
    if content.contains("RSD") {
        pre_qualifiers.push("RSD");
    }

    // Detect post-format qualifiers (go after format, in specific order)
    let mut post_qualifiers = Vec::new();

    // Mono/Stereo (mutually exclusive, check both)
    if content.contains("Mono") {
        post_qualifiers.push("Mono");
    } else if content.contains("Stereo") {
        post_qualifiers.push("Stereo");
    }

    // EP
    if content.contains("EP") {
        post_qualifiers.push("EP");
    }

    // WLP or Promo
    if content.contains("WLP") {
        post_qualifiers.push("WLP");
    } else if content.contains("Promo") {
        post_qualifiers.push("Promo Pressing");
    }

    // Build the pressing_info string
    let mut parts = vec![country];
    parts.extend(pre_qualifiers);
    parts.push(format);
    parts.extend(post_qualifiers);

    let result = parts.join(" ") + "  24-96";
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audiophile_label_detection() {
        let info = detect_pressing_info("Album (MFSL)", None);
        assert!(info.is_audiophile);
        assert_eq!(info.pressing_info, "MFSL LP  24-96");
    }
    
    #[test]
    fn test_west_german_pre_1990() {
        let info = detect_pressing_info("Album (German)", Some("1985"));
        assert_eq!(info.pressing_info, "West German First-Press LP  24-96");
    }
    
    #[test]
    fn test_german_post_1990() {
        let info = detect_pressing_info("Album (German)", Some("1995"));
        assert_eq!(info.pressing_info, "German First-Press LP  24-96"); // Post-1990 stays "German"
    }
    
    #[test]
    fn test_reissue_detection() {
        let info = detect_pressing_info("Album (2022 Reissue)", None);
        assert!(info.is_reissue);
        assert_eq!(info.pressing_info, "2022 Reissue LP  24-96");
    }
    
    #[test]
    fn test_uk_pressing() {
        let info = detect_pressing_info("Album (UK RL)", None);
        assert_eq!(info.pressing_info, "UK RL-Mastered LP  24-96");
        assert_eq!(info.country, Some("UK".to_string()));
    }

    #[test]
    fn test_minimax_standalone() {
        let info = detect_pressing_info("Album (Minimax)", None);
        assert_eq!(info.pressing_info, "Minimax LP  24-96");
    }

    #[test]
    fn test_minimax_with_country() {
        let info = detect_pressing_info("Album (UK Minimax)", None);
        assert_eq!(info.pressing_info, "UK Minimax First-Press LP  24-96");
        // Dynamic fallback for unlisted country
        let info2 = detect_pressing_info("Album (Japan Minimax)", None);
        assert_eq!(info2.pressing_info, "Japan Minimax First-Press LP  24-96");
    }

    #[test]
    fn test_red_wax() {
        let info = detect_pressing_info("Album (Red Wax)", None);
        assert_eq!(info.pressing_info, "Japan Red Wax First-Press LP  24-96");
        let info2 = detect_pressing_info("Album (Red Wax Mono)", None);
        assert_eq!(info2.pressing_info, "Japan Red Wax Mono First-Press LP  24-96");
    }

    #[test]
    fn test_german_odeon() {
        let info = detect_pressing_info("Album (German Odeon)", None);
        assert_eq!(info.pressing_info, "West German Odeon First-Press LP  24-96");
    }

    #[test]
    fn test_horzu() {
        let info = detect_pressing_info("Album (Horzu)", None);
        assert_eq!(info.pressing_info, "West German Horzu First-Press LP  24-96");
    }

    #[test]
    fn test_dynamic_year_reissue() {
        let info = detect_pressing_info("Album (2016 Reissue)", None);
        assert!(info.is_reissue);
        assert_eq!(info.pressing_info, "2016 Reissue LP  24-96");
    }

    #[test]
    fn test_dynamic_country_year_reissue() {
        let info = detect_pressing_info("Album (UK 2021 Reissue)", None);
        assert!(info.is_reissue);
        assert_eq!(info.pressing_info, "UK 2021 Reissue LP  24-96");
    }

    #[test]
    fn test_label_reissues() {
        let info = detect_pressing_info("Album (Craft Reissue)", None);
        assert_eq!(info.pressing_info, "Craft Reissue LP  24-96");
        let info2 = detect_pressing_info("Album (Studio One Reissue)", None);
        assert_eq!(info2.pressing_info, "Studio One Reissue LP  24-96");
        let info3 = detect_pressing_info("Album (Matador Reissue)", None);
        assert_eq!(info3.pressing_info, "Matador Reissue LP  24-96");
    }

    #[test]
    fn test_colored_vinyl_with_country() {
        let info = detect_pressing_info("Album (UK White Vinyl)", None);
        assert!(info.is_reissue);
        assert_eq!(info.pressing_info, "UK White Vinyl Reissue LP  24-96");
    }

    #[test]
    fn test_colored_vinyl_with_year() {
        let info = detect_pressing_info("Album (2024 Blue Vinyl)", None);
        assert!(info.is_reissue);
        assert_eq!(info.pressing_info, "US 2024 Blue Vinyl Reissue LP  24-96");
    }

    #[test]
    fn test_colored_vinyl_bare() {
        let info = detect_pressing_info("Album (Gold Vinyl)", None);
        assert!(info.is_reissue);
        assert_eq!(info.pressing_info, "US Gold Vinyl Reissue LP  24-96");
    }

    // --- Exact mapping tests for new audiophile variants ---

    #[test]
    fn test_mfsl_45_rpm_exact() {
        let info = detect_pressing_info("Album (MFSL 45 RPM)", None);
        assert_eq!(info.pressing_info, "MFSL 45-RPM Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_mofi_45_exact() {
        let info = detect_pressing_info("Album (MOFI 45)", None);
        assert_eq!(info.pressing_info, "MFSL 45-RPM Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_mofi_45_rpm_exact() {
        let info = detect_pressing_info("Album (MOFI 45 RPM)", None);
        assert_eq!(info.pressing_info, "MFSL 45-RPM Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_ap_45_rpm_exact() {
        let info = detect_pressing_info("Album (AP 45 RPM)", None);
        assert_eq!(info.pressing_info, "Analogue Productions 45-RPM Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_ap_33_rpm_exact() {
        let info = detect_pressing_info("Album (AP 33 RPM)", None);
        assert_eq!(info.pressing_info, "Analogue Productions Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_dcc_compact_classics_exact() {
        let info = detect_pressing_info("Album (DCC Compact Classics)", None);
        assert_eq!(info.pressing_info, "DCC LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_mm_33_rpm_exact() {
        let info = detect_pressing_info("Album (MM 33 RPM)", None);
        assert_eq!(info.pressing_info, "Music Matters 33-RPM Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    // --- Fallback tests for unknown audiophile variants ---

    #[test]
    fn test_mfsl_unknown_variant_fallback() {
        let info = detect_pressing_info("Album (MFSL Special Edition)", None);
        assert_eq!(info.pressing_info, "MFSL LP  24-96");
        assert!(info.is_audiophile);
        assert!(info.is_reissue);
    }

    #[test]
    fn test_dcc_unknown_variant_fallback() {
        let info = detect_pressing_info("Album (DCC Gold)", None);
        assert_eq!(info.pressing_info, "DCC LP  24-96");
        assert!(info.is_audiophile);
        assert!(info.is_reissue);
    }

    #[test]
    fn test_ap_unknown_variant_fallback() {
        let info = detect_pressing_info("Album (AP Special Edition)", None);
        assert_eq!(info.pressing_info, "Analogue Productions Reissue LP  24-96");
        assert!(info.is_audiophile);
        assert!(info.is_reissue);
    }

    #[test]
    fn test_analogue_productions_fallback_longest_first() {
        // "Analogue Productions 45 RPM" should match "Analogue Productions" prefix,
        // NOT "AP" — verifies longest-first ordering
        let info = detect_pressing_info("Album (Analogue Productions 45 RPM)", None);
        assert_eq!(info.pressing_info, "Analogue Productions Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_classic_records_fallback() {
        let info = detect_pressing_info("Album (Classic Records 200g)", None);
        assert_eq!(info.pressing_info, "Classic Records Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_speakers_corner_fallback() {
        let info = detect_pressing_info("Album (Speakers Corner 180g)", None);
        assert_eq!(info.pressing_info, "Speakers Corner Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    // --- Boundary tests: no false positives ---

    #[test]
    fn test_no_false_positive_from_ap_prefix() {
        // "Apple" should NOT match "AP" prefix (no word boundary after "AP")
        let info = detect_pressing_info("Album (Apple Records)", None);
        assert!(!info.is_audiophile);
    }

    #[test]
    fn test_random_label_no_audiophile_fallback() {
        let info = detect_pressing_info("Album (US SomeRandomLabel)", None);
        assert!(!info.is_audiophile);
    }

    // --- Priority test: exact match wins over fallback ---

    #[test]
    fn test_mfsl_45_exact_wins_over_fallback() {
        // "MFSL 45" has an exact LABEL_MAPPINGS entry — should use it, not the fallback
        let info = detect_pressing_info("Album (MFSL 45)", None);
        assert_eq!(info.pressing_info, "MFSL 45-RPM Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    // --- Short-form and newly-added label tests ---

    #[test]
    fn test_classic_short_form() {
        let info = detect_pressing_info("Album (Classic)", None);
        assert_eq!(info.pressing_info, "Classic Records Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_acoustic_sounds() {
        let info = detect_pressing_info("Album (Acoustic Sounds)", None);
        assert_eq!(info.pressing_info, "Acoustic Sounds Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_acoustic_sounds_fallback() {
        let info = detect_pressing_info("Album (Acoustic Sounds Series)", None);
        assert_eq!(info.pressing_info, "Acoustic Sounds Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_mov() {
        let info = detect_pressing_info("Album (MOV)", None);
        assert_eq!(info.pressing_info, "Music On Vinyl Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_music_on_vinyl_full() {
        let info = detect_pressing_info("Album (Music On Vinyl)", None);
        assert_eq!(info.pressing_info, "Music On Vinyl Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_bn_classic() {
        let info = detect_pressing_info("Album (BN Classic)", None);
        assert_eq!(info.pressing_info, "Blue Note Classic Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_bn_unknown_variant_fallback() {
        // "BN Something" should fall back to BN Classic base via prefix
        let info = detect_pressing_info("Album (BN Selects)", None);
        assert_eq!(info.pressing_info, "Blue Note Classic Reissue LP  24-96");
        assert!(info.is_audiophile);
    }

    #[test]
    fn test_csm_rl() {
        let info = detect_pressing_info("Album (CSM RL)", None);
        assert_eq!(info.pressing_info, "US CSM/RL-Mastered LP  24-96");
    }

    #[test]
    fn test_csm_wally() {
        let info = detect_pressing_info("Album (CSM Wally)", None);
        assert_eq!(info.pressing_info, "US CSM/Wally-Mastered LP  24-96");
    }

    #[test]
    fn test_wlp_mono() {
        let info = detect_pressing_info("Album (WLP Mono)", None);
        assert_eq!(info.pressing_info, "US White-Label Promo Mono LP  24-96");
    }

    #[test]
    fn test_sweden() {
        let info = detect_pressing_info("Album (Sweden)", None);
        assert_eq!(info.pressing_info, "Sweden First-Press LP  24-96");
    }

    #[test]
    fn test_finland() {
        let info = detect_pressing_info("Album (Finland)", None);
        assert_eq!(info.pressing_info, "Finland First-Press LP  24-96");
    }

    #[test]
    fn test_teldec() {
        let info = detect_pressing_info("Album (Teldec)", None);
        assert_eq!(info.pressing_info, "West German Teldec First-Press LP  24-96");
    }

    #[test]
    fn test_high_roller() {
        let info = detect_pressing_info("Album (High Roller)", None);
        assert_eq!(info.pressing_info, "High Roller Records Reissue LP  24-96");
    }
}