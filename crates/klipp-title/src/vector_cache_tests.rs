//! Evaluated-path cache correctness and a reproducible 4K benchmark (#2059).
use super::*;

#[path = "../tests/common/vector_case.rs"]
mod vector_case;
use vector_case::ring;

fn path(title: &mut TitleData) -> &mut klipp_timeline::VectorPath {
    let TitleObjectContent::Shape(s) = &mut title.objects[0].content else { unreachable!() };
    let ShapeKind::Path(p) = &mut s.kind else { unreachable!() }; p
}
fn sprite_id(r: &mut TitleRenderer, title: &TitleData, t:f32) -> u64 {
    match r.frame_with_window(title, 320, 180, None, t, Some(7.0)) {
        TitleFrame::Placed {sprite,..} => sprite.id,
        _ => panic!("resolved path should use a bounded cached sprite"),
    }
}
#[test]
fn completed_closed_reveal_reuses_sprite_but_partial_and_open_paths_do_not() {
    let mut title=ring(); let mut r=TitleRenderer::new();
    assert_ne!(sprite_id(&mut r,&title,0.3),sprite_id(&mut r,&title,0.8));
    let done=sprite_id(&mut r,&title,2.0);
    assert_eq!(done,sprite_id(&mut r,&title,4.0));
    assert_eq!(done,sprite_id(&mut r,&title,6.0));
    path(&mut title).pose.contours[0].closed=false;
    assert_ne!(sprite_id(&mut r,&title,2.0),sprite_id(&mut r,&title,4.0));
    for i in 0..30 { sprite_id(&mut r,&title,i as f32/10.0); }
    assert!(r.render_cache.len()<=RENDER_CACHE_CAP);
    path(&mut title).pose.stroke_glow.as_mut().unwrap().radius=12.0;
    let changed=sprite_id(&mut r,&title,4.0);
    path(&mut title).pose.stroke[0]=0.9;
    assert_ne!(changed,sprite_id(&mut r,&title,4.0));
}
#[test]
fn evaluated_paths_match_uncached_raster_including_morph_and_mixed_contours() {
    for variant in 0..4 {
        let mut title=ring();
        if variant==1 { path(&mut title).pose.contours[0].closed=false; }
        if variant==2 {
            let p=path(&mut title); let mut open=p.pose.contours[0].clone();open.closed=false;
            p.pose.contours.push(open);
        }
        if variant==3 {
            let p=path(&mut title); let a=p.pose.clone(); let mut b=a.clone();
            b.stroke_glow.as_mut().unwrap().radius=14.0;
            b.contours[0].vertices[0].point[0]=0.7;
            p.keyframes=serde_json::from_value(serde_json::json!([
                {"time_secs":0,"pose":a,"easing":"Linear"},
                {"time_secs":7,"pose":b,"easing":"Linear"}])).unwrap();
        }
        let mut r=TitleRenderer::new();let mut reference=TitleRenderer::new();
        for t in [0.0,0.4,1.0,1.9,2.0,4.0,6.0,0.4] {
            let expected=reference.raster_at(&title,320,180,None,Some(t)).to_bytes();
            let actual=r.render_with_time(&title,320,180,None,t);
            assert_eq!(actual,expected,"variant {variant}, time {t}");
        }
    }
}
#[test]
fn sliding_blur_matches_reference_at_edges_and_large_radii() {
    for (w,h) in [(1,1),(1,19),(21,1),(37,23),(513,137)] {
        let input:Vec<_>=(0..w*h).map(|i|((i*73+i/7)%256) as u8).collect();
        for radius in [0.0,1.0,2.6,8.0,50.0] {
            assert_eq!(blur_alpha(&input,w,h,radius),reference_blur(&input,w,h,radius),"{w}x{h} radius {radius}");
        }
    }
}
#[test]
#[ignore = "manual 4K performance measurement"]
fn measure_4k_vector_reveal_and_hold() {
    let title=ring();let mut r=TitleRenderer::new();
    for (label,start) in [("active reveal",0.1),("completed reveal",3.0)] {
        let mut times=Vec::new();
        for i in 0..16 {
            let t=start+i as f32/30.0;let now=std::time::Instant::now();
            std::hint::black_box(r.frame_with_window(&title,3840,2160,None,t,Some(7.0)));
            if i>0 { times.push(now.elapsed().as_secs_f64()*1000.0); }
        }
        times.sort_by(f64::total_cmp);
        println!("{label}: median {:.2} ms, max {:.2} ms",times[times.len()/2],times[times.len()-1]);
    }
    let input:Vec<_>=(0..3840*2160).map(|i|if i%3840<20 {150} else {0}).collect();
    for (name,f) in [("reference blur",reference_blur as fn(&[u8],usize,usize,f32)->Vec<u8>),("sliding blur",blur_alpha)] {
        let start=std::time::Instant::now();std::hint::black_box(f(&input,3840,2160,16.0));
        println!("{name}: {:.2} ms",start.elapsed().as_secs_f64()*1000.0);
    }
}

fn reference_blur(mask: &[u8], w: usize, h: usize, radius: f32) -> Vec<u8> {
    let r = radius.round() as usize;
    if r == 0 || w == 0 || h == 0 {
        return mask.to_vec();
    }
    let mut src = mask.to_vec();
    let mut temp = vec![0u8; w * h];
    let mut prefix = vec![0u64; w.max(h) + 1];
    // Same three box passes, edge normalization and integer rounding as
    // before, using prefix sums so the work no longer scales with radius.
    for _ in 0..3 {
        for y in 0..h {
            prefix[0] = 0;
            for x in 0..w {
                prefix[x + 1] = prefix[x] + src[y * w + x] as u64;
            }
            for x in 0..w {
                let lo = x.saturating_sub(r);
                let hi = (x + r + 1).min(w);
                temp[y * w + x] = ((prefix[hi] - prefix[lo]) / (hi - lo) as u64) as u8;
            }
        }
        for x in 0..w {
            prefix[0] = 0;
            for y in 0..h {
                prefix[y + 1] = prefix[y] + temp[y * w + x] as u64;
            }
            for y in 0..h {
                let lo = y.saturating_sub(r);
                let hi = (y + r + 1).min(h);
                src[y * w + x] = ((prefix[hi] - prefix[lo]) / (hi - lo) as u64) as u8;
            }
        }
    }
    src
}
