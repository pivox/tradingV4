//! Aléa à compteur de `model::rng` (docs/specs/s10.md §8) : vecteurs épinglés, propriétés.
//!
//! Les vecteurs de `draw` ont été calculés par le script Python de référence ci-dessous
//! (arithmétique sur 64 bits, aucune donnée de marché) ; ceux de `SplitMix64` sont les vecteurs
//! publiés avec l'implémentation de référence de Vigna. Ce test tourne sur x86-64 en CI et sur
//! aarch64 (Mac du propriétaire, ou QEMU) avec les mêmes valeurs : l'arithmétique est entière, et
//! la conversion en `[0, 1)` est exacte.
//!
//! ```python
//! M = (1 << 64) - 1
//! G = 0x9E3779B97F4A7C15
//! def mix(z):
//!     z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9 & M
//!     z = (z ^ (z >> 27)) * 0x94D049BB133111EB & M
//!     return z ^ (z >> 31)
//! def draw(seed, words):
//!     h = seed
//!     for w in words:
//!         h = mix(((h ^ w) + G) & M)
//!     return h
//! def unit(x):
//!     return (x >> 11) / 9007199254740992.0
//! ```

use tv4::model::rng::{
    GOLDEN_GAMMA, STREAM_MAKER_FILL, STREAM_STOP_SLIPPAGE, STREAM_STOP_STRESS, SplitMix64, draw,
    leg_key, mix, ts_word, unit,
};

#[test]
fn splitmix64_sequentiel_vecteurs_de_reference() {
    let mut rng = SplitMix64::new(1_234_567);
    let sorties: Vec<u64> = (0..5).map(|_| rng.next_u64()).collect();
    assert_eq!(
        sorties,
        [
            6_457_827_717_110_365_317,
            3_203_168_211_198_807_973,
            9_817_491_932_198_370_423,
            4_593_380_528_125_082_431,
            16_408_922_859_458_223_821,
        ]
    );
    assert_eq!(SplitMix64::new(0).next_u64(), 0xe220_a839_7b1d_cdaf);
}

#[test]
fn draw_vecteurs_calcules_par_le_script_de_reference() {
    let cas: [(u64, [u64; 4], u64, f64); 4] = [
        (
            0,
            [0, 0, 0, 0],
            0x2130_748a_aac8_0268,
            0.129_645_618_299_747_4,
        ),
        (
            1_234_567,
            [0x0123_4567_89AB_CDEF, 1, 1_609_459_200_000, 0],
            0x9623_3269_0dcc_e221,
            0.586_474_562_309_195_1,
        ),
        (
            1_234_567,
            [0x0123_4567_89AB_CDEF, 2, 1_609_459_200_000, 0],
            0x4caf_3e35_9a7d_077f,
            0.299_548_996_054_443_96,
        ),
        (
            1_234_567,
            [
                0x0123_4567_89AB_CDEF,
                3,
                1_609_459_260_000,
                0x40F8_6A00_0000_0000,
            ],
            0xf8b9_c01f_3738_4307,
            0.971_584_327_336_331_4,
        ),
    ];
    for (seed, words, attendu, attendu_unit) in cas {
        let x = draw(seed, words);
        assert_eq!(x, attendu, "draw({seed}, {words:x?})");
        assert_eq!(unit(x), attendu_unit, "unit de {x:#x}");
    }
}

#[test]
fn le_mot_de_prix_du_vecteur_est_le_bit_pattern_de_100000() {
    // `0x40F86A0000000000` est `100000.0_f64.to_bits()` (s10 §8.3).
    assert_eq!(100_000.0_f64.to_bits(), 0x40F8_6A00_0000_0000);
}

#[test]
fn les_flux_sont_ceux_de_la_spec() {
    assert_eq!(STREAM_STOP_SLIPPAGE, 1);
    assert_eq!(STREAM_STOP_STRESS, 2);
    assert_eq!(STREAM_MAKER_FILL, 3);
    assert_eq!(GOLDEN_GAMMA, 0x9E37_79B9_7F4A_7C15);
}

#[test]
fn unit_reste_dans_zero_un_exclu() {
    assert_eq!(unit(0), 0.0);
    // Le plus grand tirage possible reste strictement sous 1 : 1 − 2^-53.
    let max = unit(u64::MAX);
    assert!(max < 1.0);
    assert_eq!(max, 1.0 - 1.0 / 9_007_199_254_740_992.0);
    // Les 11 bits de poids faible sont ignorés.
    assert_eq!(unit(0x7FF), 0.0);
    assert_eq!(unit(0x800), 1.0 / 9_007_199_254_740_992.0);
    let mut rng = SplitMix64::new(42);
    for _ in 0..10_000 {
        let u = rng.next_unit();
        assert!((0.0..1.0).contains(&u), "{u}");
    }
}

#[test]
fn leg_key_est_stable_et_vient_de_blake3() {
    // blake3("") = af1349b9f5f9a1a6…, publié avec la crate : les 8 premiers octets en petit-boutiste.
    assert_eq!(leg_key(""), 0xa6a1_f9f5_b949_13af);
    // Valeurs épinglées à partir de la sortie de `blake3` sur le symbole en UTF-8.
    assert_eq!(leg_key("BTC-USD"), 0xcc5f_cbdd_f6f8_fa2a);
    assert_eq!(leg_key("ETH-USD"), 0xf290_022d_a011_ebdc);
    assert_eq!(leg_key("BTC-USDC"), 0x69e2_5259_f82b_055f);
    assert_eq!(leg_key("BTC-USD"), leg_key("BTC-USD"));
    assert_ne!(leg_key("BTC-USD"), leg_key("BTC-USDC"));
}

#[test]
fn deux_graines_donnent_des_tirages_differents_sur_mille_cles() {
    let key = leg_key("BTC-USD");
    let mut different = 0;
    for i in 0..1_000_i64 {
        let words = [
            key,
            STREAM_STOP_SLIPPAGE,
            ts_word(1_609_459_200_000 + i * 60_000),
            0,
        ];
        if draw(1, words) != draw(2, words) {
            different += 1;
        }
    }
    assert_eq!(different, 1_000);
}

#[test]
fn un_tirage_est_une_fonction_pure_de_sa_cle() {
    let words = [leg_key("ETH-USD"), STREAM_MAKER_FILL, 1_700_000_000_000, 7];
    let x = draw(1, words);
    // Aucun état caché : rappeler dans un autre ordre, ou après d'autres tirages, redonne la valeur.
    for autre in 0..50 {
        let _ = draw(1, [autre, 1, 2, 3]);
    }
    assert_eq!(draw(1, words), x);
    // L'ordre des mots compte, comme chaque mot.
    let mut permute = words;
    permute.swap(1, 2);
    assert_ne!(draw(1, permute), x);
    for i in 0..4 {
        let mut modifie = words;
        modifie[i] ^= 1;
        assert_ne!(draw(1, modifie), x, "mot {i}");
    }
}

#[test]
fn les_horodatages_entrent_en_complement_a_deux() {
    assert_eq!(ts_word(0), 0);
    assert_eq!(ts_word(-1), u64::MAX);
    assert_eq!(ts_word(1_609_459_200_000), 1_609_459_200_000);
}

#[test]
fn mix_est_la_fonction_de_splitmix64() {
    // Première sortie de graine 0 : mix(0 + G).
    assert_eq!(mix(GOLDEN_GAMMA), 0xe220_a839_7b1d_cdaf);
}
