use klipp_analysis::{AnalysisCtx, Analyser, motion};
#[test]
fn held_vfr_pictures_are_measured_stillness() {
    let ctx=AnalysisCtx::default();if !ctx.ffmpeg_available(){return;}
    let path=std::env::temp_dir().join(format!("klipp-vfr-motion-{}.mkv",std::process::id()));
    let status=std::process::Command::new(&ctx.ffmpeg).args(["-y","-v","error","-f","lavfi","-i","color=blue:s=160x90:r=30:d=7","-vf","select='eq(n,0)+eq(n,90)+eq(n,180)'","-fps_mode","vfr","-c:v","ffv1"]).arg(&path).status().unwrap();assert!(status.success());
    let mut item=klipp_media::MediaItem::synthetic_for_test(path.clone(),None);item.video_codec="ffv1".into();item.duration=klipp_core::Timecode::from_secs_f64(6.0,30);
    let result=motion::MotionAnalyser.run(&item,&ctx).unwrap();let m:motion::Motion=serde_json::from_value(result).unwrap();
    assert_eq!(m.class_within(1.0,5.0),Some("static"));assert!(m.per_second[1..5].iter().all(|v|*v==Some(0.0)),"{:?}",m.per_second);
    std::fs::remove_file(path).unwrap();
}
