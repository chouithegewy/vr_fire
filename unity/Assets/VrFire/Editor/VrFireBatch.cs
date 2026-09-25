// Headless (batch-mode) pipeline for the vr_fire Unity project:
//
//   Unity -batchmode -quit -projectPath unity -executeMethod VrFire.EditorTools.VrFireBatch.All
//
// ImportTiles  copies ../bench/tiles/lod0/*.fbx (vr_fire bake --format fbx) into Assets/VrFire/Tiles
// Verify       checks each imported tile against its raw heights (count, scale, orientation)
// BuildScene   URP asset (4x MSAA, no shadows), light, benchmark camera, tiles from scene.json
// BuildLinux   standalone Linux player in Build/Linux
//
// Options: -tiles <dir with .fbx>  -heights <dir with .f32>  -scene <scene.json>  -out <player path>

using System;
using System.IO;
using System.Linq;
using UnityEditor;
using UnityEditor.Build.Reporting;
using UnityEditor.SceneManagement;
using UnityEngine;
using UnityEngine.Rendering;
using UnityEngine.Rendering.Universal;

namespace VrFire.EditorTools
{
    /// Import settings for terrain tiles: metres, imported normals, 32-bit indices, no extras.
    class TileImport : AssetPostprocessor
    {
        void OnPreprocessModel()
        {
            if (!assetPath.StartsWith(VrFireBatch.TilesDir)) return;
            var m = (ModelImporter)assetImporter;
            m.useFileScale = true; // the FBX says 1 unit = 100 cm
            m.globalScale = 1f;
            m.bakeAxisConversion = false;
            m.meshCompression = ModelImporterMeshCompression.Off;
            m.indexFormat = ModelImporterIndexFormat.UInt32;
            m.importNormals = ModelImporterNormals.Import;
            m.importTangents = ModelImporterTangents.None;
            m.materialImportMode = ModelImporterMaterialImportMode.None;
            m.importAnimation = false;
            m.importCameras = false;
            m.importLights = false;
            m.importBlendShapes = false;
            m.addCollider = false;
            m.isReadable = false;
        }
    }

    public static class VrFireBatch
    {
        public const string TilesDir = "Assets/VrFire/Tiles";
        const string SettingsDir = "Assets/VrFire/Settings";
        const string ScenePath = "Assets/VrFire/Scenes/Bench.unity";
        const int Nodes = 376;
        const float TileM = 3750f;

        internal static string RepoRoot => Path.GetFullPath(Path.Combine(Application.dataPath, "..", ".."));

        internal static string Arg(string name, string fallback)
        {
            var args = Environment.GetCommandLineArgs();
            int i = Array.IndexOf(args, name);
            return i >= 0 && i + 1 < args.Length ? args[i + 1] : fallback;
        }

        internal static void Fail(string msg)
        {
            Debug.LogError("VRFIRE FAIL: " + msg);
            if (Application.isBatchMode) EditorApplication.Exit(1);
            throw new Exception(msg);
        }

        public static void ImportTiles()
        {
            var src = Arg("-tiles", Path.Combine(RepoRoot, "bench", "tiles", "lod0"));
            var files = Directory.Exists(src) ? Directory.GetFiles(src, "*.fbx") : Array.Empty<string>();
            if (files.Length == 0) Fail($"no .fbx tiles in {src}; run `vr_fire bake --format fbx --out bench/tiles`");
            Directory.CreateDirectory(TilesDir);
            foreach (var f in files) File.Copy(f, Path.Combine(TilesDir, Path.GetFileName(f)), true);
            AssetDatabase.Refresh(ImportAssetOptions.ForceSynchronousImport);
            Debug.Log($"VRFIRE imported {files.Length} tiles from {src}");
        }

        /// Imported meshes must match the raw heights: NW corner at the origin, +x east, +z north.
        public static void Verify()
        {
            var heightsDir = Arg("-heights", Path.Combine(RepoRoot, "bench", "tiles"));
            var guids = AssetDatabase.FindAssets("t:Model", new[] { TilesDir });
            if (guids.Length == 0) Fail("no imported tiles to verify");
            foreach (var guid in guids)
            {
                var path = AssetDatabase.GUIDToAssetPath(guid);
                var name = Path.GetFileNameWithoutExtension(path);
                var root = AssetDatabase.LoadAssetAtPath<GameObject>(path);
                var mf = root.GetComponentInChildren<MeshFilter>();
                if (mf == null || mf.sharedMesh == null) Fail($"{name}: no mesh");
                var mesh = mf.sharedMesh;
                var toRoot = root.transform.worldToLocalMatrix * mf.transform.localToWorldMatrix;
                if (mesh.indexFormat != IndexFormat.UInt32) Fail($"{name}: 16-bit indices");
                var verts = mesh.vertices.Select(v => toRoot.MultiplyPoint3x4(v)).ToArray();
                var raw = File.ReadAllBytes(Path.Combine(heightsDir, name + ".f32"));
                float H(int row, int col) => BitConverter.ToSingle(raw, (row * Nodes + col) * 4);
                // (east, north) of each corner in Unity, and its height in the raw grid.
                var corners = new (string, float, float, float)[]
                {
                    ("NW", 0f, 0f, H(0, 0)),
                    ("NE", TileM, 0f, H(0, Nodes - 1)),
                    ("SW", 0f, -TileM, H(Nodes - 1, 0)),
                    ("SE", TileM, -TileM, H(Nodes - 1, Nodes - 1)),
                };
                foreach (var (label, x, z, want) in corners)
                {
                    // Skirt vertices share the corner's x/z but hang lower: take the highest.
                    var near = verts.Where(v => Mathf.Abs(v.x - x) < 0.5f && Mathf.Abs(v.z - z) < 0.5f).ToArray();
                    if (near.Length == 0) Fail($"{name}: no vertex at {label} corner ({x}, {z}); bounds {mesh.bounds}");
                    float got = near.Max(v => v.y);
                    if (Mathf.Abs(got - want) > 0.01f) Fail($"{name}: {label} height {got} != {want}");
                }
                Debug.Log($"VRFIRE verified {name}: {mesh.vertexCount} vertices, {mesh.GetIndexCount(0) / 3} triangles, corners match");
            }
        }

        internal static UniversalRenderPipelineAsset MakePipeline()
        {
            Directory.CreateDirectory(SettingsDir);
            var rendererPath = SettingsDir + "/BenchRenderer.asset";
            var pipelinePath = SettingsDir + "/BenchURP.asset";
            AssetDatabase.DeleteAsset(pipelinePath);
            AssetDatabase.DeleteAsset(rendererPath);
            var renderer = ScriptableObject.CreateInstance<UniversalRendererData>();
            AssetDatabase.CreateAsset(renderer, rendererPath);
            var urp = UniversalRenderPipelineAsset.Create(renderer);
            urp.msaaSampleCount = 4;
            urp.supportsHDR = false;
            urp.shadowDistance = 0f;
            AssetDatabase.CreateAsset(urp, pipelinePath);
            GraphicsSettings.defaultRenderPipeline = urp;
            for (int i = 0; i < QualitySettings.names.Length; i++)
            {
                QualitySettings.SetQualityLevel(i, false);
                QualitySettings.renderPipeline = urp;
                QualitySettings.vSyncCount = 0;
            }
            return urp;
        }

        public static void BuildScene()
        {
            var scenePath = Arg("-scene", Path.Combine(RepoRoot, "bench", "scene.json"));
            var json = File.ReadAllText(scenePath);
            var scene = JsonUtility.FromJson<SceneFile>(json);
            Directory.CreateDirectory("Assets/StreamingAssets");
            File.WriteAllText("Assets/StreamingAssets/bench_scene.json", json);

            MakePipeline();
            var s = EditorSceneManager.NewScene(NewSceneSetup.EmptyScene, NewSceneMode.Single);
            RenderSettings.ambientMode = AmbientMode.Flat;
            RenderSettings.ambientLight = new Color(0.45f, 0.47f, 0.5f);

            var sun = new GameObject("Sun").AddComponent<Light>();
            sun.type = LightType.Directional;
            sun.intensity = 1.2f;
            sun.shadows = LightShadows.None;
            sun.transform.rotation = Quaternion.Euler(50f, -30f, 0f);

            var camGo = new GameObject("BenchCamera") { tag = "MainCamera" };
            var cam = camGo.AddComponent<Camera>();
            cam.fieldOfView = 60f;
            cam.nearClipPlane = 1f;
            cam.farClipPlane = 30000f;
            cam.allowMSAA = true;
            cam.clearFlags = CameraClearFlags.SolidColor;
            cam.backgroundColor = new Color(0.55f, 0.7f, 0.9f);
            camGo.AddComponent<Benchmark>();

            var mat = new Material(Shader.Find("Universal Render Pipeline/Lit")) { name = "Terrain" };
            mat.SetColor("_BaseColor", new Color(0.42f, 0.45f, 0.33f));
            mat.SetFloat("_Smoothness", 0.1f);
            AssetDatabase.CreateAsset(mat, SettingsDir + "/Terrain.mat");

            foreach (var t in scene.tiles)
            {
                var model = AssetDatabase.LoadAssetAtPath<GameObject>($"{TilesDir}/{t.name}.fbx");
                if (model == null) Fail($"tile {t.name} is not imported");
                var go = (GameObject)PrefabUtility.InstantiatePrefab(model);
                go.transform.position = new Vector3(t.east_m, 0f, -t.south_m);
                foreach (var r in go.GetComponentsInChildren<MeshRenderer>())
                {
                    r.sharedMaterial = mat;
                    r.shadowCastingMode = ShadowCastingMode.Off;
                    r.receiveShadows = false;
                }
            }
            Directory.CreateDirectory(Path.GetDirectoryName(ScenePath));
            EditorSceneManager.SaveScene(s, ScenePath);
            EditorBuildSettings.scenes = new[] { new EditorBuildSettingsScene(ScenePath, true) };

            PlayerSettings.productName = "vr_fire bench";
            PlayerSettings.companyName = "vr_fire";
            PlayerSettings.fullScreenMode = FullScreenMode.Windowed;
            PlayerSettings.defaultScreenWidth = scene.resolution[0];
            PlayerSettings.defaultScreenHeight = scene.resolution[1];
            PlayerSettings.resizableWindow = false;
            PlayerSettings.runInBackground = true;
            PlayerSettings.enableFrameTimingStats = true;
            // Vulkan first (as Bevy uses on Linux); OpenGL only as a fallback.
            PlayerSettings.SetUseDefaultGraphicsAPIs(BuildTarget.StandaloneLinux64, false);
            PlayerSettings.SetGraphicsAPIs(BuildTarget.StandaloneLinux64, new[] { GraphicsDeviceType.Vulkan, GraphicsDeviceType.OpenGLCore });
            AssetDatabase.SaveAssets();
            Debug.Log($"VRFIRE scene {ScenePath}: {scene.tiles.Length} tiles");
        }

        /// OpenXR creates its settings asset from the Editor UI and fails any build that starts
        /// without it ("Please build again"). Create and register it up front for batch builds.
        /// No XR loader is enabled for Standalone, so the benchmark player stays flat-screen.
        public static void PrepareXR()
        {
            var t = Type.GetType("UnityEditor.XR.OpenXR.OpenXRPackageSettings, Unity.XR.OpenXR.Editor");
            var create = t?.GetMethod("GetOrCreateInstance", System.Reflection.BindingFlags.Public | System.Reflection.BindingFlags.Static);
            if (create == null) Fail("OpenXR package settings API not found");
            create.Invoke(null, null);
            AssetDatabase.SaveAssets();
        }

        public static void BuildLinux()
        {
            var output = Arg("-out", Path.Combine(Application.dataPath, "..", "Build", "Linux", "vr_fire_bench.x86_64"));
            PrepareXR();
            var report = BuildPipeline.BuildPlayer(new BuildPlayerOptions
            {
                scenes = new[] { ScenePath },
                locationPathName = output,
                target = BuildTarget.StandaloneLinux64,
                options = BuildOptions.None,
            });
            var sum = report.summary;
            if (sum.result != BuildResult.Succeeded) Fail($"build {sum.result}: {sum.totalErrors} errors");
            Debug.Log($"VRFIRE built {output} ({sum.totalSize / 1e6:0.0} MB) in {sum.totalTime.TotalSeconds:0}s");
        }

        public static void All()
        {
            ImportTiles();
            Verify();
            BuildScene();
            BuildLinux();
        }

        [Serializable]
        class SceneTile
        {
            public string name;
            public float east_m, south_m;
        }

        [Serializable]
        class SceneFile
        {
            public SceneTile[] tiles;
            public int[] resolution;
        }
    }
}
