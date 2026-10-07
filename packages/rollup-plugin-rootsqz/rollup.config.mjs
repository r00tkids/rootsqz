import typescript from '@rollup/plugin-typescript';
const config = [
    {
        input: 'src/index.ts',
        external: ['@rollup/pluginutils', '@r00tkids/rootsqz', /^node:.*/],
        output: {
            dir: 'dist',
            format: 'esm',
            sourcemap: true,
        },
        plugins: [
            typescript({
                declaration: true,
            }),
        ],
    }
];

export default config;