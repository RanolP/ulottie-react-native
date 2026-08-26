import React, { useState } from 'react';
import { Pressable, StyleSheet, Text, View } from 'react-native';
import { StatusBar } from 'expo-status-bar';
import ParityScreen from './src/ParityScreen';
import PerfScreen from './src/PerfScreen';

export default function App() {
  const [tab, setTab] = useState('parity');
  return (
    <View style={styles.root}>
      <StatusBar style="dark" />
      <View style={styles.tabs}>
        <Pressable
          testID="tab-parity"
          onPress={() => setTab('parity')}
          style={[styles.tab, tab === 'parity' && styles.tabSelected]}
        >
          <Text style={styles.tabText}>Parity</Text>
        </Pressable>
        <Pressable
          testID="tab-perf"
          onPress={() => setTab('perf')}
          style={[styles.tab, tab === 'perf' && styles.tabSelected]}
        >
          <Text style={styles.tabText}>Perf</Text>
        </Pressable>
      </View>
      {tab === 'parity' ? <ParityScreen /> : <PerfScreen />}
    </View>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: '#ffffff', paddingTop: 60 },
  tabs: { flexDirection: 'row', gap: 8, paddingHorizontal: 8, paddingBottom: 4 },
  tab: {
    paddingHorizontal: 14,
    paddingVertical: 8,
    borderRadius: 6,
    backgroundColor: '#eeeeee',
  },
  tabSelected: { backgroundColor: '#ccddff' },
  tabText: { fontSize: 14, color: '#222222' },
});
