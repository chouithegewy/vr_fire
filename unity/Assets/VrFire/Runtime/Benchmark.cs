// Flies the shared benchmark path (../bench/scene.json, copied to StreamingAssets) and writes
// frame-time statistics as JSON, then quits. The same path runs in the Bevy benchmark, so the
// numbers are comparable.
//
// Coordinates in scene.json are metres east and south of the first tile's NW corner. Unity is
// left-handed with +z north, so a point (east, south) is (east, h, -south) here.

using System;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Linq;
using System.Text;
using UnityEngine;

namespace VrFire
{
    [Serializable]
    public class Orbit
    {
        public float centre_east_m, centre_south_m, radius_m, altitude_m, look_at_height_m, turns;
    }

    [Serializable]
    public class BenchScene
    {
        public float warmup_s, duration_s;
        public int[] resolution;
        public Orbit orbit;
    }

    public class Benchmark : MonoBehaviour
    {
        BenchScene cfg;
        float start;
        readonly List<double> frameMs = new List<double>();
        readonly List<double> cpuMs = new List<double>();
        readonly List<double> gpuMs = new List<double>();
        readonly FrameTiming[] timing = new FrameTiming[1];
        bool done, shot;

        static string Arg(string name)
        {
            var args = Environment.GetCommandLineArgs();
            int i = Array.IndexOf(args, name);
            return i >= 0 && i + 1 < args.Length ? args[i + 1] : null;
        }

        void Start()
        {
            QualitySettings.vSyncCount = 0;
            Application.targetFrameRate = -1;
            cfg = JsonUtility.FromJson<BenchScene>(File.ReadAllText(Path.Combine(Application.streamingAssetsPath, "bench_scene.json")));
            Screen.SetResolution(cfg.resolution[0], cfg.resolution[1], FullScreenMode.Windowed);
            start = Time.realtimeSinceStartup;
            Place(0f);
        }

        /// Camera pose at fraction `u` of the orbit (identical to the Bevy benchmark).
        public static void Pose(Orbit o, float u, out Vector3 eye, out Vector3 look)
        {
            float a = u * o.turns * 2f * Mathf.PI;
            float east = o.centre_east_m + Mathf.Cos(a) * o.radius_m;
            float south = o.centre_south_m + Mathf.Sin(a) * o.radius_m;
            eye = new Vector3(east, o.altitude_m, -south);
            look = new Vector3(o.centre_east_m, o.look_at_height_m, -o.centre_south_m);
        }

        void Place(float u)
        {
            Pose(cfg.orbit, u, out var eye, out var look);
            transform.SetPositionAndRotation(eye, Quaternion.LookRotation(look - eye, Vector3.up));
        }

        void Update()
        {
            if (done || cfg == null) return;
            float t = Time.realtimeSinceStartup - start;
            Place(Mathf.Clamp01((t - cfg.warmup_s) / cfg.duration_s));
            if (t <= cfg.warmup_s) return;
            // One screenshot a quarter of the way round, to check what was measured.
            var shotPath = Arg("-benchShot");
            if (!shot && shotPath != null && t > cfg.warmup_s + cfg.duration_s * 0.25f)
            {
                shot = true;
                ScreenCapture.CaptureScreenshot(shotPath);
            }
            frameMs.Add(Time.unscaledDeltaTime * 1000.0);
            FrameTimingManager.CaptureFrameTimings();
            if (FrameTimingManager.GetLatestTimings(1, timing) > 0)
            {
                if (timing[0].cpuFrameTime > 0) cpuMs.Add(timing[0].cpuFrameTime);
                if (timing[0].gpuFrameTime > 0) gpuMs.Add(timing[0].gpuFrameTime);
            }
            if (t > cfg.warmup_s + cfg.duration_s)
            {
                done = true;
                Write();
                Application.Quit();
            }
        }

        static double Pct(List<double> v, double p)
        {
            if (v.Count == 0) return double.NaN;
            var s = v.OrderBy(x => x).ToList();
            int rank = (int)Math.Ceiling(p * s.Count);
            return s[Math.Clamp(rank, 1, s.Count) - 1];
        }

        static string Stats(List<double> v)
        {
            if (v.Count == 0) return "null";
            string f(double x) => x.ToString("0.###", CultureInfo.InvariantCulture);
            return $"{{\"mean\": {f(v.Average())}, \"p50\": {f(Pct(v, 0.5))}, \"p95\": {f(Pct(v, 0.95))}, \"p99\": {f(Pct(v, 0.99))}, \"max\": {f(v.Max())}}}";
        }

        void Write()
        {
            long tris = 0;
            foreach (var mf in FindObjectsByType<MeshFilter>(FindObjectsSortMode.None))
                if (mf.sharedMesh != null)
                    for (int i = 0; i < mf.sharedMesh.subMeshCount; i++) tris += (long)mf.sharedMesh.GetIndexCount(i) / 3;
            var json = new StringBuilder();
            json.Append("{\n");
            json.Append($"  \"engine\": \"Unity {Application.unityVersion}\",\n");
            json.Append($"  \"graphics_api\": \"{SystemInfo.graphicsDeviceType}\",\n");
            json.Append($"  \"gpu\": \"{SystemInfo.graphicsDeviceName}\",\n");
            json.Append($"  \"resolution\": [{Screen.width}, {Screen.height}],\n");
            var urp = UnityEngine.Rendering.GraphicsSettings.currentRenderPipeline as UnityEngine.Rendering.Universal.UniversalRenderPipelineAsset;
            json.Append($"  \"msaa\": {(urp != null ? urp.msaaSampleCount : QualitySettings.antiAliasing)},\n");
            json.Append($"  \"triangles\": {tris},\n");
            json.Append($"  \"frames\": {frameMs.Count},\n");
            json.Append($"  \"fps_mean\": {(1000.0 / frameMs.Average()).ToString("0.#", CultureInfo.InvariantCulture)},\n");
            json.Append($"  \"frame_ms\": {Stats(frameMs)},\n");
            json.Append($"  \"cpu_ms\": {Stats(cpuMs)},\n");
            json.Append($"  \"gpu_ms\": {Stats(gpuMs)}\n");
            json.Append("}\n");
            var path = Arg("-benchOut") ?? Path.Combine(Application.persistentDataPath, "unity_bench.json");
            File.WriteAllText(path, json.ToString());
            Debug.Log($"BENCH_RESULT {path}\n{json}");
        }
    }
}
