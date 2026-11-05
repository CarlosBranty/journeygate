// C# ground truth for the impact-vs-codegraph benchmark.
//
// usage: dotnet gt_csharp.dll <repo> <out.json>
//
// For every SDK-style .csproj under <repo> it builds a CSharpCompilation the
// way MSBuild would feed csc, without a restore:
//   * Compile items: the default **/*.cs glob of the project directory (minus
//     bin/, obj/, nested project directories and MAUI Platforms/<other>/),
//     plus unconditioned <Compile Include>, minus <Compile Remove>;
//   * global usings: the SDK's implicit usings when ImplicitUsings is enabled,
//     plus unconditioned <Using> items (csproj, Directory.Build.props/targets);
//   * preprocessor symbols of one target framework (a plain netX.Y when the
//     project lists one, else the first) plus DEBUG/TRACE and DefineConstants;
//   * metadata references: the reference assemblies of the installed
//     Microsoft.NETCore.App.Ref and Microsoft.AspNetCore.App.Ref 10.x packs;
//   * ProjectReferences (transitively, like the SDK) as CompilationReferences.
// NuGet packages are not referenced: their types do not bind, which is fine
// because they are never repository types.
//
// Edge semantics (same as guard-impact's C# plugin): file A -> file B when a
// simple name in A (IdentifierName, GenericName, also the parts of a
// QualifiedName, in any position: types, object creation, attributes, typeof,
// nameof, static member access, using static / using alias directives) binds,
// through SemanticModel.GetSymbolInfo (the symbol, else every candidate), to a
// named type declared in source, or to a constructor of one (attributes);
// the edge goes to every file of the type's DeclaringSyntaxReferences (each
// part of a partial type). An implicit `var` is not a name. Extension method
// calls, type parameters and inferred types name no type. Disabled #if
// branches and doc comments are trivia and are not visited.
//
// The universe is every compiled file that is not generated (*.g.cs,
// *.g.i.cs, *.designer.cs, *.generated.cs, or an "auto-generated" tag in the first 2 KB).
// Generated files still take part in the compilations, so names bind.

using System.Text.Json;
using System.Text.RegularExpressions;
using System.Xml.Linq;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

if (args.Length != 2)
{
    Console.Error.WriteLine("usage: gt_csharp <repo> <out.json>");
    return 2;
}

var repo = Path.GetFullPath(args[0]).TrimEnd('\\', '/');
string Norm(string p) => Path.GetFullPath(p).Replace('\\', '/');
string Rel(string full) => Path.GetRelativePath(repo, full).Replace('\\', '/');

var files = Walk(repo).ToList();
var projectPaths = files.Where(f => f.EndsWith(".csproj", StringComparison.OrdinalIgnoreCase)).ToList();
var projectDirs = projectPaths.Select(p => Norm(Path.GetDirectoryName(p)!)).ToList();
var csFiles = files.Where(f => f.EndsWith(".cs", StringComparison.OrdinalIgnoreCase)).Select(Norm).ToList();

var references = ReferenceAssemblies();
var projects = projectPaths.Select(p => Project.Load(Norm(p), repo)).ToDictionary(p => p.Path);
var compilations = new Dictionary<string, CSharpCompilation>();
var compiledBy = new Dictionary<string, List<string>>();
var errors = new SortedDictionary<string, int>();

CSharpCompilation Compile(Project p, HashSet<string> stack)
{
    if (compilations.TryGetValue(p.Path, out var done)) return done;
    if (!stack.Add(p.Path)) throw new InvalidOperationException($"ProjectReference cycle at {p.Path}");
    var refs = new List<MetadataReference>(references);
    foreach (var dep in Closure(p))
        refs.Add(Compile(projects[dep], stack).ToMetadataReference());
    var parse = new CSharpParseOptions(LanguageVersion.Preview, DocumentationMode.Parse, SourceCodeKind.Regular, p.Symbols());
    var sources = p.CompileItems(csFiles, projectDirs);
    var trees = sources.Select(f => CSharpSyntaxTree.ParseText(File.ReadAllText(f), parse, f)).ToList();
    var usings = p.GlobalUsings();
    if (usings.Length > 0)
        trees.Add(CSharpSyntaxTree.ParseText(usings, parse, Path.Combine(p.Dir, "obj", "__gt_global_usings.g.cs")));
    foreach (var f in sources)
    {
        if (!compiledBy.TryGetValue(f, out var by)) compiledBy[f] = by = new List<string>();
        by.Add(p.Path);
    }
    var options = new CSharpCompilationOptions(p.IsExe ? OutputKind.ConsoleApplication : OutputKind.DynamicallyLinkedLibrary,
        nullableContextOptions: NullableContextOptions.Enable, allowUnsafe: true);
    var c = CSharpCompilation.Create(p.AssemblyName, trees, refs, options);
    compilations[p.Path] = c;
    stack.Remove(p.Path);
    return c;
}

IEnumerable<string> Closure(Project p)
{
    var seen = new SortedSet<string>(StringComparer.Ordinal);
    var todo = new Stack<string>(p.ProjectReferences.Where(projects.ContainsKey));
    while (todo.Count > 0)
    {
        var x = todo.Pop();
        if (!seen.Add(x)) continue;
        foreach (var y in projects[x].ProjectReferences.Where(projects.ContainsKey)) todo.Push(y);
    }
    return seen;
}

foreach (var p in projects.Values.OrderBy(p => p.Path, StringComparer.Ordinal)) Compile(p, new HashSet<string>());

var universe = new SortedSet<string>(compiledBy.Keys.Where(f => !IsGenerated(f)).Select(Rel), StringComparer.Ordinal);
var edges = new SortedSet<(string, string)>();
// Completeness check: unbound names (CS0246/CS0103/CS0234) spelled like a repository type.
var repoTypeNames = new HashSet<string>(compilations.Values.SelectMany(c => c.SyntaxTrees)
    .Where(t => universe.Contains(Rel(t.FilePath)))
    .SelectMany(t => t.GetRoot().DescendantNodes())
    .Select(n => n switch { BaseTypeDeclarationSyntax b => b.Identifier.Text, DelegateDeclarationSyntax d => d.Identifier.Text, _ => null })
    .OfType<string>(), StringComparer.Ordinal);
var unboundRepoNames = new SortedSet<string>(StringComparer.Ordinal);
var projectInfo = new List<object>();
foreach (var (path, c) in compilations.OrderBy(kv => kv.Key, StringComparer.Ordinal))
{
    foreach (var d in c.GetDiagnostics().Where(d => d.Severity == DiagnosticSeverity.Error))
    {
        errors[d.Id] = errors.GetValueOrDefault(d.Id) + 1;
        if (d.Id is "CS0246" or "CS0103" or "CS0234" && d.Location.SourceTree is { } st && universe.Contains(Rel(st.FilePath)))
        {
            var text = st.GetText().ToString(d.Location.SourceSpan);
            var simple = text.Split('.', '<')[^1].Split('<')[0];
            if (repoTypeNames.Contains(text.Split('<')[0].Split('.')[^1]) || repoTypeNames.Contains(simple))
                unboundRepoNames.Add($"{Rel(st.FilePath)}:{d.Location.GetLineSpan().StartLinePosition.Line + 1}:{d.Id}:{text}");
        }
    }
    int own = 0;
    foreach (var tree in c.SyntaxTrees)
    {
        var from = Rel(tree.FilePath);
        if (!universe.Contains(from)) continue;
        own++;
        var model = c.GetSemanticModel(tree);
        foreach (var name in tree.GetRoot().DescendantNodes().OfType<SimpleNameSyntax>())
        {
            var info = model.GetSymbolInfo(name);
            var symbols = info.Symbol is { } s ? new[] { s } : info.CandidateSymbols.ToArray();
            foreach (var symbol in symbols)
            {
                var target = symbol is IAliasSymbol alias ? alias.Target : symbol;
                var type = target switch
                {
                    INamedTypeSymbol t => t,
                    IMethodSymbol { MethodKind: MethodKind.Constructor } m => m.ContainingType,
                    _ => null,
                };
                if (type is null || type.TypeKind == TypeKind.Error) continue;
                if (name is IdentifierNameSyntax { IsVar: true } && type.Name != "var") continue;
                foreach (var decl in type.OriginalDefinition.DeclaringSyntaxReferences)
                {
                    var to = Rel(decl.SyntaxTree.FilePath);
                    if (to != from && universe.Contains(to)) edges.Add((from, to));
                }
            }
        }
    }
    var proj = projects[path];
    projectInfo.Add(new { path = Rel(path), sdk = proj.Sdk, tfm = proj.Tfm, files = own, project_references = Closure(proj).Count() });
}

var output = new
{
    tool = $"roslyn {typeof(CSharpCompilation).Assembly.GetName().Version} SemanticModel.GetSymbolInfo over SimpleNameSyntax",
    universe,
    edges = edges.Select(e => new[] { e.Item1, e.Item2 }),
    projects = projectInfo,
    compile_errors = errors,
    unbound_repo_type_names = unboundRepoNames,
};
File.WriteAllText(args[1], JsonSerializer.Serialize(output));
Console.WriteLine($"projects={compilations.Count} universe={universe.Count} edges={edges.Count} errors={errors.Values.Sum()} unbound_repo_type_names={unboundRepoNames.Count}");
return 0;

static IEnumerable<string> Walk(string dir)
{
    foreach (var f in Directory.EnumerateFiles(dir)) yield return f;
    foreach (var d in Directory.EnumerateDirectories(dir))
    {
        var n = Path.GetFileName(d);
        if (n.StartsWith('.') || n is "bin" or "obj" or "node_modules") continue;
        foreach (var f in Walk(d)) yield return f;
    }
}

static bool IsGenerated(string path)
{
    var n = Path.GetFileName(path).ToLowerInvariant();
    if (n.EndsWith(".g.cs") || n.EndsWith(".g.i.cs") || n.EndsWith(".designer.cs") || n.EndsWith(".generated.cs")) return true;
    using var r = new StreamReader(path);
    var buf = new char[2048];
    var head = new string(buf, 0, r.Read(buf, 0, buf.Length));
    return head.Contains("<auto-generated", StringComparison.OrdinalIgnoreCase);
}

static List<MetadataReference> ReferenceAssemblies()
{
    // <dotnet>/shared/Microsoft.NETCore.App/<ver>/System.Private.CoreLib.dll -> <dotnet>
    var root = Path.GetFullPath(Path.Combine(Path.GetDirectoryName(typeof(object).Assembly.Location)!, "..", "..", ".."));
    var list = new List<MetadataReference>();
    foreach (var pack in new[] { "Microsoft.NETCore.App.Ref", "Microsoft.AspNetCore.App.Ref" })
    {
        var versions = Directory.GetDirectories(Path.Combine(root, "packs", pack))
            .Where(d => Path.GetFileName(d).StartsWith("10."))
            .OrderBy(d => Version.Parse(Path.GetFileName(d).Split('-')[0]))
            .ToList();
        if (versions.Count == 0) throw new InvalidOperationException($"no 10.x {pack} under {root}");
        foreach (var dll in Directory.GetFiles(Path.Combine(versions[^1], "ref", "net10.0"), "*.dll"))
            list.Add(MetadataReference.CreateFromFile(dll));
    }
    return list;
}

sealed class Project
{
    public required string Path;
    public required string Dir;
    public required string Sdk;
    public required Dictionary<string, string> Props;
    public required List<XElement> Items;

    public string AssemblyName => Props.GetValueOrDefault("AssemblyName") ?? System.IO.Path.GetFileNameWithoutExtension(Path);
    public IEnumerable<string> ProjectReferences => Items.Where(i => i.Name.LocalName == "ProjectReference" && i.Attribute("Include") != null)
        .Select(i => System.IO.Path.GetFullPath(System.IO.Path.Combine(Dir, Unslash(i.Attribute("Include")!.Value))).Replace('\\', '/'));
    bool Flag(string name) => Props.GetValueOrDefault(name)?.Trim().ToLowerInvariant() is "true" or "enable";
    bool Maui => Flag("UseMaui");

    public bool IsExe => (Props.GetValueOrDefault("OutputType") ?? "").Contains("Exe", StringComparison.OrdinalIgnoreCase)
        || Sdk is "Microsoft.NET.Sdk.Web" or "Microsoft.NET.Sdk.Worker" or "Aspire.AppHost.Sdk";

    public string Tfm
    {
        get
        {
            var all = (Props.GetValueOrDefault("TargetFramework") ?? Props.GetValueOrDefault("TargetFrameworks") ?? "net10.0")
                .Split(';', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
                .Where(t => !t.Contains("$(")).ToList();
            return all.FirstOrDefault(t => !t.Contains('-')) ?? all.FirstOrDefault() ?? "net10.0";
        }
    }

    string? Platform => Tfm.Contains('-') ? Regex.Match(Tfm.Split('-')[1], "^[a-z]+").Value : null;

    public static Project Load(string path, string repo)
    {
        var dir = System.IO.Path.GetDirectoryName(path)!.Replace('\\', '/');
        var props = new Dictionary<string, string>();
        var items = new List<XElement>();
        var doc = XDocument.Load(path);
        var sdk = (doc.Root!.Attribute("Sdk")?.Value ?? "Microsoft.NET.Sdk").Split('/')[0];
        // Directory.Build.props (nearest, then the ones it explicitly chains to), the project, Directory.Build.targets.
        foreach (var f in BuildFiles(dir, repo, "Directory.Build.props").Reverse()) Apply(XDocument.Load(f), props, items);
        Apply(doc, props, items);
        foreach (var f in BuildFiles(dir, repo, "Directory.Build.targets").Reverse()) Apply(XDocument.Load(f), props, items);
        return new Project { Path = path, Dir = dir, Sdk = sdk, Props = props, Items = items };
    }

    static IEnumerable<string> BuildFiles(string dir, string repo, string name)
    {
        var d = new DirectoryInfo(dir);
        var stop = System.IO.Path.GetFullPath(repo);
        while (d != null && System.IO.Path.GetFullPath(d.FullName).Length >= stop.Length)
        {
            var f = System.IO.Path.Combine(d.FullName, name);
            if (File.Exists(f))
            {
                yield return f;
                if (!File.ReadAllText(f).Contains("GetPathOfFileAbove", StringComparison.Ordinal)) yield break;
            }
            d = d.Parent;
        }
    }

    static void Apply(XDocument doc, Dictionary<string, string> props, List<XElement> items)
    {
        foreach (var g in doc.Root!.Elements().Where(e => e.Attribute("Condition") == null))
        {
            if (g.Name.LocalName == "PropertyGroup")
                foreach (var p in g.Elements().Where(e => e.Attribute("Condition") == null))
                    props[p.Name.LocalName] = p.Value.Contains($"$({p.Name.LocalName})")
                        ? p.Value.Replace($"$({p.Name.LocalName})", props.GetValueOrDefault(p.Name.LocalName) ?? "")
                        : p.Value;
            else if (g.Name.LocalName == "ItemGroup")
                items.AddRange(g.Elements().Where(e => e.Attribute("Condition") == null));
        }
    }

    static string Unslash(string p) => p.Replace('\\', '/');

    public string[] Symbols()
    {
        var s = new List<string> { "DEBUG", "TRACE" };
        var m = Regex.Match(Tfm, @"^net(\d+)\.(\d+)");
        if (m.Success)
        {
            int major = int.Parse(m.Groups[1].Value), minor = int.Parse(m.Groups[2].Value);
            s.AddRange(["NET", "NETCOREAPP", $"NET{major}_{minor}"]);
            for (var v = 5; v <= major; v++) s.Add($"NET{v}_0_OR_GREATER");
            s.AddRange(new[] { "1_0", "1_1", "2_0", "2_1", "2_2", "3_0", "3_1" }.Select(v => $"NETCOREAPP{v}_OR_GREATER"));
        }
        if (Platform is { } plat) s.Add(plat.ToUpperInvariant());
        foreach (var c in (Props.GetValueOrDefault("DefineConstants") ?? "").Split(';', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
            if (!c.Contains("$(")) s.Add(c);
        return s.ToArray();
    }

    static readonly string[] BaseUsings = ["System", "System.Collections.Generic", "System.IO", "System.Linq", "System.Net.Http", "System.Threading", "System.Threading.Tasks"];
    static readonly string[] HostingUsings = ["Microsoft.Extensions.Configuration", "Microsoft.Extensions.DependencyInjection", "Microsoft.Extensions.Hosting", "Microsoft.Extensions.Logging"];
    static readonly string[] WebUsings = ["System.Net.Http.Json", "Microsoft.AspNetCore.Builder", "Microsoft.AspNetCore.Hosting", "Microsoft.AspNetCore.Http", "Microsoft.AspNetCore.Routing", .. HostingUsings];
    static readonly string[] MauiUsings = ["Microsoft.Extensions.DependencyInjection", "Microsoft.Maui", "Microsoft.Maui.Controls", "Microsoft.Maui.Controls.Hosting", "Microsoft.Maui.Controls.Xaml", "Microsoft.Maui.Graphics", "Microsoft.Maui.Hosting", "Microsoft.Maui.ApplicationModel", "Microsoft.Maui.ApplicationModel.Communication", "Microsoft.Maui.ApplicationModel.DataTransfer", "Microsoft.Maui.Authentication", "Microsoft.Maui.Devices", "Microsoft.Maui.Devices.Sensors", "Microsoft.Maui.Media", "Microsoft.Maui.Networking", "Microsoft.Maui.Storage", "Microsoft.Maui.Accessibility"];

    public string GlobalUsings()
    {
        // (kind, name, alias): kind 0 namespace, 1 static, 2 alias
        var list = new List<(int, string, string?)>();
        if (Flag("ImplicitUsings"))
        {
            var ns = new List<string>(BaseUsings);
            if (Sdk is "Microsoft.NET.Sdk.Web") ns.AddRange(WebUsings);
            if (Sdk is "Microsoft.NET.Sdk.Worker") ns.AddRange(HostingUsings);
            if (Sdk is "Aspire.AppHost.Sdk") ns.AddRange(["Aspire.Hosting", "Aspire.Hosting.ApplicationModel"]);
            if (Maui) ns.AddRange(MauiUsings);
            list.AddRange(ns.Select(n => (0, n, (string?)null)));
        }
        if (Sdk is "MSTest.Sdk") list.Add((0, "Microsoft.VisualStudio.TestTools.UnitTesting", null));
        foreach (var u in Items.Where(i => i.Name.LocalName == "Using"))
        {
            var alias = u.Attribute("Alias")?.Value;
            var isStatic = (u.Attribute("Static")?.Value ?? "").Equals("true", StringComparison.OrdinalIgnoreCase);
            if (u.Attribute("Remove")?.Value is { } rm) { list.RemoveAll(x => x.Item2 == rm); continue; }
            if (u.Attribute("Include")?.Value is { } inc) list.Add((alias != null ? 2 : isStatic ? 1 : 0, inc, alias));
        }
        return string.Join("\n", list.Distinct().Select(x => x.Item1 switch
        {
            1 => $"global using static global::{x.Item2};",
            2 => $"global using {x.Item3} = global::{x.Item2};",
            _ => $"global using global::{x.Item2};",
        }));
    }

    public List<string> CompileItems(List<string> csFiles, List<string> projectDirs)
    {
        var set = new SortedSet<string>(StringComparer.Ordinal);
        var defaults = (Props.GetValueOrDefault("EnableDefaultCompileItems") ?? "true").Trim().ToLowerInvariant() != "false";
        if (defaults)
        {
            var nested = projectDirs.Where(d => d != Dir && d.StartsWith(Dir + "/", StringComparison.Ordinal)).ToList();
            foreach (var f in csFiles)
            {
                if (!f.StartsWith(Dir + "/", StringComparison.Ordinal)) continue;
                if (nested.Any(n => f.StartsWith(n + "/", StringComparison.Ordinal))) continue;
                var rel = f[(Dir.Length + 1)..];
                if (Maui && rel.StartsWith("Platforms/", StringComparison.Ordinal))
                {
                    var platDir = rel.Split('/')[1].ToLowerInvariant();
                    if (Platform is null || platDir != Platform) continue;
                }
                set.Add(f);
            }
        }
        foreach (var item in Items.Where(i => i.Name.LocalName == "Compile"))
        {
            if (item.Attribute("Include")?.Value is { } inc)
                foreach (var f in Match(inc, csFiles)) set.Add(f);
            if (item.Attribute("Remove")?.Value is { } rm)
                foreach (var f in Match(rm, csFiles)) set.Remove(f);
        }
        return set.ToList();
    }

    IEnumerable<string> Match(string spec, List<string> csFiles)
    {
        foreach (var part in spec.Split(';', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries))
        {
            if (part.Contains("$(")) continue;
            var full = System.IO.Path.GetFullPath(System.IO.Path.Combine(Dir, Unslash(part))).Replace('\\', '/');
            if (!full.Contains('*') && !full.Contains('?'))
            {
                if (File.Exists(full)) yield return full;
                continue;
            }
            var rx = new Regex("^" + Regex.Escape(full).Replace(@"\*\*/", "(.*/)?").Replace(@"\*", "[^/]*").Replace(@"\?", ".") + "$", RegexOptions.IgnoreCase);
            foreach (var f in csFiles.Where(f => rx.IsMatch(f))) yield return f;
        }
    }
}
