// Project-owned TypeScript AST rules: intentionally smaller than a general-purpose linter.
import ts from 'typescript';
import { readdirSync, readFileSync } from 'node:fs';
import { join, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
/**
 * Authorization vocabulary. In the live adapter, any identifier that names
 * authorization (authorize, authenticate, permission, privilege, credentials,
 * allow/deny lists, access control, ACLs) means it is deciding instead of
 * transporting: the broker owns every such decision.
 */
export const authorization = /authori[sz]|authenticat|permission|privilege|credential|allow_?list|deny_?list|access_?control|is_?allowed|can_?access|^acls?$|Acls?(?![a-z])/i;
/**
 * Fields of the frozen domain contract (AppState, Settings) whose names use
 * that vocabulary. The live adapter must fill them, with nothing, to build a
 * complete snapshot: naming one as an object-literal key is not a
 * decision. Reading one, or naming anything else so, still is.
 */
export const contractFields = new Set(['permissions', 'websitePermissions', 'networkPermissions', 'allowlist', 'credentialReference']);
const fillsContractField = node => contractFields.has(node.text) && ts.isPropertyAssignment(node.parent) && node.parent.name === node;
/** Exported so boundary rules are tested against violating source, not merely their own output. */
export function lintSource(path, text) {
    const diagnostics = [];
    const file = ts.createSourceFile(path, text, ts.ScriptTarget.Latest, true);
    const report = (node, message) => {
        const position = file.getLineAndCharacterOfPosition(node.getStart(file));
        diagnostics.push(`${path}:${position.line + 1}:${position.character + 1} ${message}`);
    };
    if (/@ts-(ignore|nocheck|expect-error)|eslint-disable/.test(text))
        report(file, 'Suppression comments are forbidden.');
    const presentation = /src\/(?:features|app|components|presentation)\//.test(path);
    // The live adapter is a thin transport: authorization lives in the
    // broker. Its generated wire types are data, checked by protocol:check.
    const live = /src\/adapters\/live\//.test(path) && !path.endsWith('src/adapters/live/protocol/generated.ts');
    const visit = (node) => {
        if (presentation && ts.isImportDeclaration(node) && ts.isStringLiteral(node.moduleSpecifier) && node.moduleSpecifier.text.includes('protocol/') && !node.importClause?.isTypeOnly)
            report(node, 'Presentation may import only contract types, not protocol implementations.');
        if (presentation && ts.isExportDeclaration(node) && node.moduleSpecifier && ts.isStringLiteral(node.moduleSpecifier) && /adapters\/|protocol\//.test(node.moduleSpecifier.text))
            report(node, 'Presentation cannot re-export protocol or adapter implementations.');
        if (presentation && ts.isCallExpression(node) && node.expression.kind === ts.SyntaxKind.ImportKeyword)
            report(node, 'Dynamic imports in presentation require an explicit boundary review.');
        if (node.kind === ts.SyntaxKind.AnyKeyword)
            report(node, 'Explicit any is forbidden. Narrow unknown instead.');
        if (ts.isNonNullExpression(node))
            report(node, 'Non-null assertion is forbidden. Handle absence.');
        if (ts.isAsExpression(node) && node.type.getText(file) !== 'const')
            report(node, 'Unchecked type casts are forbidden. Use a type guard.');
        if (ts.isTypeAssertionExpression(node))
            report(node, 'Unchecked type casts are forbidden.');
        if (ts.isCallExpression(node) && ['eval', 'Function'].includes(node.expression.getText(file)))
            report(node, 'Dynamic code execution is forbidden.');
        if (ts.isPropertyAccessExpression(node) && ['innerHTML', 'outerHTML'].includes(node.name.text))
            report(node, 'HTML string injection is forbidden. Construct DOM nodes.');
        if (ts.isBinaryExpression(node) && [ts.SyntaxKind.EqualsEqualsToken, ts.SyntaxKind.ExclamationEqualsToken].includes(node.operatorToken.kind))
            report(node, 'Loose equality is forbidden. Use === or !==.');
        if (ts.isPropertyAccessExpression(node) && ts.isIdentifier(node.expression) && node.expression.text === 'console')
            report(node, 'Console output is forbidden in application code.');
        if (ts.isExportAssignment(node) || (ts.canHaveModifiers(node) && ts.getModifiers(node)?.some(modifier => modifier.kind === ts.SyntaxKind.DefaultKeyword)))
            report(node, 'Default exports are forbidden. Export names.');
        if (ts.isVariableDeclarationList(node) && !(node.flags & (ts.NodeFlags.Let | ts.NodeFlags.Const)))
            report(node, 'var is forbidden. Use const or let.');
        if (ts.isTypeReferenceNode(node) && ts.isIdentifier(node.typeName) && ['Function', 'Object'].includes(node.typeName.text))
            report(node, 'The Function and Object types are forbidden. Name the shape.');
        if (node.kind === ts.SyntaxKind.DebuggerStatement)
            report(node, 'debugger statements are forbidden.');
        if (ts.isForInStatement(node))
            report(node, 'for...in is forbidden. Iterate Object.keys or Object.entries.');
        if (ts.isDeleteExpression(node))
            report(node, 'delete is forbidden. Build a new value.');
        if (ts.isLabeledStatement(node))
            report(node, 'Labels are forbidden.');
        if (ts.isFunctionDeclaration(node) && node.body && !node.type && ts.getModifiers(node)?.some(modifier => modifier.kind === ts.SyntaxKind.ExportKeyword))
            report(node, 'Exported functions must declare their return type.');
        if (live && ts.isCatchClause(node) && node.block.statements.length === 0)
            report(node, 'Empty catch blocks are forbidden in the live adapter. Handle the failure or rethrow it.');
        if (live && ts.isPrefixUnaryExpression(node) && node.operator === ts.SyntaxKind.ExclamationToken && ts.isPrefixUnaryExpression(node.operand) && node.operand.operator === ts.SyntaxKind.ExclamationToken)
            report(node, 'Double negation is forbidden in the live adapter. Compare explicitly.');
        if (live && ts.isIdentifier(node) && authorization.test(node.text) && !fillsContractField(node))
            report(node, 'Authorization belongs to the broker: the live adapter is a thin transport with no authorization logic .');
        if (ts.isImportDeclaration(node) && ts.isStringLiteral(node.moduleSpecifier)) {
            const source = node.moduleSpecifier.text;
            if (!source.endsWith('.js'))
                report(node, 'Use explicit .js module extensions for native ESM.');
            // Each app's entry is its composition root; the Machine view's own
            // modules may name the operator adapter's types, never its code.
            const root = path === 'src/main.ts' || path === 'src/machine.ts';
            const machineTypes = path.startsWith('src/machine/') && node.importClause?.isTypeOnly === true;
            if (!path.includes('adapters/') && !root && !machineTypes && source.includes('adapters/'))
                report(node, 'Only the composition root may import adapter implementations.');
            if (path.includes('protocol/') && /features|components|app|adapters/.test(source))
                report(node, 'Protocol must not depend on presentation or implementations.');
        }
        if (!path.includes('adapters/') && ts.isIdentifier(node) && ['fetch', 'XMLHttpRequest', 'WebSocket', 'localStorage', 'sessionStorage'].includes(node.text))
            report(node, 'I/O belongs behind ProtocolAdapter.');
        ts.forEachChild(node, visit);
    };
    visit(file);
    return diagnostics;
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
    const walk = directory => readdirSync(directory, { withFileTypes: true }).flatMap(entry => entry.isDirectory() ? walk(join(directory, entry.name)) : [join(directory, entry.name)]);
    // The rules name paths with `/`, whatever the system's separator.
    const files = walk('src').filter(path => path.endsWith('.ts')).map(path => path.split(sep).join('/'));
    const errors = files.flatMap(path => lintSource(path, readFileSync(path, 'utf8')));
    for (const error of errors)
        console.error(error);
    console.log(`${files.length} TypeScript modules checked; ${errors.length} lint errors.`);
    if (errors.length)
        process.exitCode = 1;
}
