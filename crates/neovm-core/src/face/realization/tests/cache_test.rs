use super::*;
use crate::face::{Color, FaceAttrValue, FontWeight, LFaceAttr, SpecifiedColor};

fn source() -> Face {
    let mut face = Face::default();
    face.set_attribute(
        LFaceAttr::Foreground,
        FaceAttrValue::SpecifiedColor {
            specification: SpecifiedColor::parse("#123456"),
            realized: Color::from_hex("#123456"),
        },
    );
    face
}

#[test]
fn realization_cache_compares_complete_attributes_within_color_buckets() {
    let source = source();
    let mut first = source.clone();
    first.foreground = Some(Color::rgb(1, 2, 3));
    let mut second_source = source.clone();
    second_source.weight = Some(FontWeight::BOLD);
    let mut second = second_source.clone();
    second.foreground = Some(Color::rgb(4, 5, 6));
    let mut cache = FrameFaceRealizationCache::default();
    cache.insert(source.clone(), first.clone());
    cache.insert(second_source.clone(), second.clone());
    assert_eq!(cache.lookup(&source), Some(&first));
    assert_eq!(cache.lookup(&second_source), Some(&second));
    let mut third = source;
    third.extend = Some(true); // routing hash intentionally excludes this field
    assert!(cache.lookup(&third).is_none());
}

#[test]
fn realization_cache_generation_and_owners_are_independent() {
    let mut first = FrameFaceRealizationCache::default();
    let second = FrameFaceRealizationCache::default();
    let attributes = source();
    first.prepare(1, FaceTable::new());
    first.insert(attributes.clone(), attributes.clone());
    assert!(first.table(1).is_some());
    assert!(first.table(2).is_none());
    assert!(second.lookup(&attributes).is_none());
    first.prepare(2, FaceTable::new());
    assert!(first.lookup(&attributes).is_none());
    first.invalidate();
    assert!(first.table(2).is_none());
}

#[test]
fn realization_cache_uses_structural_lisp_attribute_equality() {
    let mut first = source();
    first.family = Some(Value::string("separately-allocated-family"));
    first.stipple = Some(Value::list(vec![
        Value::fixnum(8),
        Value::fixnum(1),
        Value::string("bits"),
    ]));
    let mut equal = first.clone();
    equal.family = Some(Value::string("separately-allocated-family"));
    equal.stipple = Some(Value::list(vec![
        Value::fixnum(8),
        Value::fixnum(1),
        Value::string("bits"),
    ]));
    assert_ne!(first.family.unwrap().bits(), equal.family.unwrap().bits());
    assert_ne!(first.stipple.unwrap().bits(), equal.stipple.unwrap().bits());
    let mut cache = FrameFaceRealizationCache::default();
    cache.insert(first.clone(), first.clone());
    assert_eq!(cache.lookup(&equal), Some(&first));
    equal.family = Some(Value::string("different-family"));
    assert!(cache.lookup(&equal).is_none());
}
